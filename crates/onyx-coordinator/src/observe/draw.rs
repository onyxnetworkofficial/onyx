//! A round's observer draw (L2-F2, L2RND-R1): announce, fix the beacon, follow the chain's draw
//! record and open, and the pure checks a client reruns (pool, draw, beacon, commit timing).

use super::*;

/// Draw a round's observers and open it (L2-F2, L2RND-R1), the way every
/// runner does: align the store with the registry's draw count, announce the
/// draw in the store (counting the attempt), persist the book (`save`),
/// announce it on chain before its beacon height, fix the beacon on chain
/// once its block exists, open with the beacon the chain gives, and keep the
/// opened round in the store and save the book again.
///
/// THE DRAW INVARIANT (review R8, ADR-0027 amendment 8 item 8f): the book
/// is saved with the draw BEFORE `announceDraw` is sent, and from then on
/// the draw leaves the book only through
/// [`ObservationResults::release_draw`], under a proof: its round or failed
/// attempt durably published, or ONE final block showing it can never land
/// (`BeaconNotFuture`/`BadAttempt`) or never have its beacon fixed
/// (`BeaconUnavailable`). Every other outcome (an error, a stale read, a
/// mismatch, a crash, a timeout) leaves it in the book, announced or kept,
/// and returns an error: the next run resumes the SAME draw. A draw the
/// book holds announced and not opened is resumed here (no second
/// announcement). Opening is one step with keeping
/// ([`ObservationResults::open_drawn`]): the registry's record is checked
/// before the round is opened, so no draw is ever opened and not kept.
///
/// A chain draw the book lacks (a lost or older book, another book's) is
/// adopted when its commitment is this runner's, the results store (read
/// first, [`ObservationResults::note_published_draws`]) does not publish it,
/// and one final block shows it as the chain's last draw
/// ([`ObservationResults::adopt_draw`], review R9-F1); the latest one the
/// results store publishes is followed by a next draw; an earlier one is drawn past when
/// its beacon is fixed (settled history: a draw gets a successor only once
/// it is released, review R8-2) or can never be fixed; the latest is drawn
/// past only once it can never be fixed; otherwise the run stops. The beacon
/// fix is idempotent ([`fix_beacon_idempotent`]). Every chain read is
/// retried per `wait`.
pub fn draw_and_open(
    store: &mut ObservationResults,
    spec: ObservationSpec,
    cfg: ObserveConfig,
    members: &[Observer],
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
    save: &mut dyn FnMut(&ObservationResults) -> Result<(), String>,
) -> Result<(ObservationRound, DrawRecord), String> {
    let fact = store.onchain_fact(&spec).map_err(|e| e.to_string())?;
    let mut onchain = wait.retry(|| chain.draw_count(&fact))?;
    // Review REVLOG-R5: a draw announced (on chain and in the saved book)
    // whose run stopped before the round opened is resumed on that draw.
    let resume = match store.pending_draw(&spec) {
        Some(d) if d.attempt.saturating_add(1) == onchain => Some(d),
        // Review R6-B: the count says the chain lacks the book's draw. One
        // `drawCount` answer (a lagging node behind a load balancer) never
        // drops it: it is confirmed by `drawOf`, and released only under a
        // final-block proof that it can never land.
        Some(d) if d.attempt >= onchain => match confirm_announced(chain, &d, wait) {
            Confirm::Landed => {
                onchain = d.attempt.saturating_add(1);
                Some(d)
            }
            Confirm::Gone(proof) => {
                release(store, &spec, proof, save)?;
                None
            }
            Confirm::Unknown(c) => {
                return Err(format!(
                    "the chain is behind the book: draw {} is in the book and {onchain} draws on chain ({c}); it stays in the book: run again with the same book",
                    d.attempt
                ))
            }
        },
        // Review R7-2: the chain holds draws after the book's pending draw
        // (another book announced them). The book's draw is not the latest,
        // so it can never be resumed; it leaves the book only under a proof
        // (it can never land, or never be fixed), then the later draws are
        // followed as for a book that lacks them (review R8-2: the latest
        // may be adopted).
        Some(d) => {
            let proof = gone_proof(chain, &d, wait).map_err(|why| {
                format!(
                    "the chain holds {onchain} draws, past the book's draw {} ({why}); it stays in the book: restore the book that announced the later draws",
                    d.attempt
                )
            })?;
            release(store, &spec, proof, save)?;
            follow_chain(store, &spec, &cfg, members, chain, &fact, onchain, wait, save)?
        }
        None => follow_chain(store, &spec, &cfg, members, chain, &fact, onchain, wait, save)?,
    };
    store
        .sync_draws(&spec, onchain)
        .map_err(|e| format!("sync draws: {e}"))?;
    // L2RND-R2-F1: the subject's bound lives on chain; refuse before
    // announcing rather than let the registry revert.
    let (strikes, allowance) = wait.retry(|| chain.draw_budget(&fact))?;
    let (draw, snapshot) = match resume {
        Some(draw) => {
            // The resumed draw is one of the chain's strikes and already
            // passed the bound when it was announced.
            store
                .sync_budget(&spec, strikes.saturating_sub(1), allowance)
                .map_err(|e| format!("sync budget: {e}"))?;
            let snapshot = store
                .announced_pool(&spec)
                .map_err(|e| format!("resume: {e}"))?;
            (draw, snapshot)
        }
        None => {
            store
                .sync_budget(&spec, strikes, allowance)
                .map_err(|e| format!("sync budget: {e}"))?;
            if strikes >= allowance {
                return Err(format!(
                    "DrawsExhausted: the subject holds {strikes} of {allowance} draws without a recorded value; the registry owner must reopen it"
                ));
            }
            let attempt = store
                .next_attempt(&spec)
                .map_err(|e| format!("announce: {e}"))?;
            if attempt != onchain {
                return Err(format!(
                    "the book is ahead of the chain: attempt {attempt} but {onchain} draws on chain"
                ));
            }
            let lead = wait.retry(|| chain.draw_lead())?;
            let height = wait.retry(|| chain.height())?.saturating_add(lead);
            let snapshot = store
                .announce_draw(&spec, &cfg, members, height, &*chain)
                .map_err(|e| format!("announce: {e}"))?;
            let draw = store.latest_draw(&spec).map_err(|e| e.to_string())?;
            // Review R7-2 (THREATS REVLOG-R5-2): the book holds the draw on
            // disk BEFORE `announceDraw` is sent, so no crash leaves a draw
            // on chain that the book lacks. A failed save sends nothing.
            if let Err(s) = save(store) {
                let back = store
                    .release_draw(&spec, Release::NeverSent)
                    .map_err(|e| format!("; {e}"))
                    .err()
                    .unwrap_or_default();
                return Err(format!(
                    "saving the book with draw {} before announcing it: {s}; nothing was sent{back}",
                    draw.attempt
                ));
            }
            // L2RND-R2-F5, review R6-A: an announcement error does not mean
            // the announcement did not land (a receipt timeout after the
            // send). The draw stays in the book until the chain shows it, or
            // a final block proves it can never land.
            if let Err(e) = chain.announce(&draw) {
                match confirm_announced(chain, &draw, wait) {
                    Confirm::Landed => {}
                    Confirm::Gone(proof) => {
                        release(store, &spec, proof, save)?;
                        return Err(format!(
                            "announce: {e}; the chain shows draw {} never landed: forgotten",
                            draw.attempt
                        ));
                    }
                    Confirm::Unknown(c) => {
                        return Err(format!(
                            "announce: {e}; draw {} is not confirmed on chain yet ({c}); it stays in the book: run again with the same book (it resumes the draw if the chain holds it, forgets it once a final block past its beacon height shows it absent)",
                            draw.attempt
                        ))
                    }
                }
            }
            (draw, snapshot)
        }
    };
    let rec = match fix_beacon_idempotent(chain, &draw, wait) {
        Ok(r) => r,
        Err(e) => return Err(beacon_not_fixed(store, &spec, chain, &draw, wait, save, e)),
    };
    // Review R8-1: opened and kept in one step, after the registry's record
    // is checked; a mismatch leaves the draw announced in the book.
    let round = open_retrying(store, &spec, cfg, snapshot, &*chain, &draw, rec, wait)
        .map_err(|e| format!("open: {e}; the draw stays announced in the book: run again with the same book to open it on the same draw"))?;
    let rec = store
        .unfinished(&spec)
        .map(|u| u.onchain_draw)
        .ok_or("the opened round is not kept")?;
    // THREATS REVLOG-R4-5: the opened round is in the book from here on, so
    // a crash in its commit phase finishes it on this draw.
    save(store).map_err(|e| {
        format!("saving the opened round: {e}; it is kept in the store: finish it on the same draw")
    })?;
    Ok((round, rec))
}

/// Release the book's draw of `spec` under `why`
/// ([`ObservationResults::release_draw`]) and save the book.
pub(super) fn release(
    store: &mut ObservationResults,
    spec: &ObservationSpec,
    why: Release,
    save: &mut dyn FnMut(&ObservationResults) -> Result<(), String>,
) -> Result<(), String> {
    store
        .release_draw(spec, why)
        .map_err(|e| format!("releasing the book's draw: {e}"))?;
    save(store)
}

/// Review R11-3: a runner read no results store file because the operator
/// passed `--new-store` (a fact's first run on a deployment). Refused when
/// the chain already shows draws of `spec`'s fact and the book holds none:
/// that is not a first run but a lost book with a missing or mistyped
/// store path (`--new-store` left on), which would read as "nothing
/// published" and could adopt a draw whose round the real store publishes
/// (REVLOG-R9-F1). A book that holds the chain's latest draw (announced,
/// opened or kept: a first run that stopped before its store was written)
/// goes on.
///
/// Review R12 (REVLOG-R12-2): the book must hold the chain's LATEST draw
/// `n - 1` (or `n`, saved before its announcement landed): a book holding
/// an earlier draw while the chain shows more is not this fact's first run
/// either. The count is the larger of the latest block's and a final
/// block's ([`draw_count_seen`]): a lagging node answering a low count at
/// `latest` cannot let the run through, since a final block's state must
/// be served as of that block or the read fails.
pub fn new_store_allowed(
    store: &ObservationResults,
    spec: &ObservationSpec,
    chain: &dyn DrawChain,
    wait: &ChainWait,
) -> Result<(), String> {
    let fact = store.onchain_fact(spec).map_err(|e| e.to_string())?;
    let n = draw_count_seen(chain, &fact, wait)?;
    let held = store.held_draw(spec).map(|d| d.attempt);
    if n > 0 && !held.is_some_and(|a| a + 1 >= n && a <= n) {
        return Err(format!(
            "--new-store: there is no results store file, but the chain already shows {n} draw(s) of this fact and the book holds {}: this is not the fact's first run on this deployment. Check the --store path; pass --new-store only on a fact's first run (review R11-3, R12)",
            held.map_or("none".to_string(), |a| format!("draw {a}, not the chain's latest"))
        ));
    }
    Ok(())
}

/// Review R12: `drawCount` of `fact` read twice, at `latest` (retried) and
/// AT a final block ([`DrawChain::draw_of_final`] of attempt 0, whose
/// record carries the count as of that block; no draw 0 there is a count
/// of 0), and the larger taken. A node behind the head can under-report the
/// latest count; the final read names its block, so a node without that
/// block's state errors instead of answering an older count.
pub fn draw_count_seen(
    chain: &dyn DrawChain,
    fact: &OnchainFact,
    wait: &ChainWait,
) -> Result<u32, String> {
    let latest = wait.retry(|| chain.draw_count(fact))?;
    let (_, first) = wait.retry(|| chain.draw_of_final(fact, 0))?;
    Ok(latest.max(first.map_or(0, |r| r.draw_count)))
}

/// The chain holds `onchain` draws and the book holds none it can resume:
/// the draws past the book's count (a lost or older book, or another
/// book's). Review R8-2: an earlier one is drawn past when its beacon is
/// fixed (settled history: a draw gets a successor only once released, so
/// its round is published) or can never be fixed; the latest is followed
/// (a next draw) when the results store publishes it (review R9-F1), else
/// adopted when its commitment is this runner's (THREATS REVLOG-R5-2) and
/// one final block shows it as the chain's last draw, else drawn past only
/// once it can never be fixed. Otherwise Err: the run stops
/// (drawing past a fixable draw leaves it for anyone to fix, and a fixed
/// beacon without a published round voids the fact). `Some(draw)`: adopted,
/// to resume.
#[allow(clippy::too_many_arguments)]
pub(super) fn follow_chain(
    store: &mut ObservationResults,
    spec: &ObservationSpec,
    cfg: &ObserveConfig,
    members: &[Observer],
    chain: &dyn DrawChain,
    fact: &OnchainFact,
    onchain: u32,
    wait: &ChainWait,
    save: &mut dyn FnMut(&ObservationResults) -> Result<(), String>,
) -> Result<Option<OnchainDraw>, String> {
    let Ok(known) = store.next_attempt(spec) else {
        return Ok(None);
    };
    if known >= onchain {
        return Ok(None);
    }
    let last = onchain - 1;
    for a in known..last {
        draw_past(chain, fact, a, wait, Past::Earlier)?;
    }
    // `drawCount` showed the draw: a lagging `drawOf` never lets the run
    // draw past it.
    let Some(rec) = wait.retry(|| chain.draw_of(fact, last))? else {
        return Err(format!(
            "drawCount shows {onchain} draws but drawOf shows no draw at attempt {last}: run again with the same book"
        ));
    };
    store
        .sync_draws(spec, last)
        .map_err(|e| format!("sync draws: {e}"))?;
    // Review R9-F1: the results store publishes the latest draw (a book
    // this one lost published its round, then was lost): it is settled
    // history, released when it was published, and the next draw follows
    // it. Adopting it would open a second, different round on a published
    // draw, which the results store refuses for good.
    //
    // Review R10-1: matched by beacon height AND beacon. A results store of
    // another deployment (fresh chains give the same heights) would
    // otherwise make the run draw past this chain's latest draw, unfixed and
    // fixable by anyone. A published draw has a fixed beacon, so the
    // chain's must be that same, nonzero beacon. (Runners also refuse a
    // results store tagged with another deployment, `DrawChain::deployment`.)
    //
    // Review R11-4: a node lagging behind the `fixBeacon` shows the draw
    // with a zero beacon: a draw that differs is read again (`wait.retry`)
    // before the run stops, and the message names both causes.
    if let Some(Some(p)) = store.published_draw(spec, last) {
        let same = |r: &DrawRecord| {
            p.beacon_height == r.beacon_height && p.beacon != Hash32::ZERO && p.beacon == r.beacon
        };
        if !same(&rec) {
            let mut seen = rec;
            let again = if p.beacon == Hash32::ZERO {
                Err("a published draw without a beacon".to_string())
            } else {
                wait.retry(|| match chain.draw_of(fact, last)? {
                    Some(r) if same(&r) => Ok(()),
                    Some(r) => {
                        seen = r;
                        Err("differs".into())
                    }
                    None => Err(format!("drawOf shows no draw at attempt {last}")),
                })
            };
            if again.is_err() {
                return Err(format!(
                    "the results store publishes draw {last} with beacon height {} and beacon {}, the chain's draw {last} still reads {} and {} ({}, review R11-4): a zero beacon is a node that has not seen fixBeacon yet (run again later, or with another node); any other is the results store of another registry (run again with this registry's results store)",
                    p.beacon_height,
                    p.beacon,
                    seen.beacon_height,
                    seen.beacon,
                    again.err().unwrap_or_default()
                ));
            }
        }
        return Ok(None);
    }
    if store
        .adoptable(spec, cfg, members, &rec)
        .map_err(|e| format!("adopt: {e}"))?
    {
        // Review R9-F1: adopted only when ONE final block shows it as the
        // chain's last draw (`drawOf` and `drawCount` of that block, the
        // count equal to the latest one read), read again on an
        // inconsistent answer: a stale or mixed read never makes an earlier
        // draw (released, so its round is published) look like the latest.
        let fin = wait
            .retry(|| match chain.draw_of_final(fact, last)? {
                (_, Some(r))
                    if r.attempt == last
                        && r.commit == rec.commit
                        && r.beacon_height == rec.beacon_height
                        && r.draw_count == onchain =>
                {
                    Ok(r)
                }
                (b, r) => Err(format!(
                    "final block {b} shows {} at attempt {last}",
                    r.map_or("no draw".into(), |r| format!(
                        "a draw with drawCount {}",
                        r.draw_count
                    ))
                )),
            })
            .map_err(|e| {
                format!(
                    "the chain's latest draw {last} was announced with this runner's inputs; it is adopted only once one final block shows it as the chain's last of {onchain} draws ({e}): run again with the same book"
                )
            })?;
        if store
            .adopt_draw(spec, cfg, members, &fin)
            .map_err(|e| format!("adopt: {e}"))?
        {
            save(store)?;
            return Ok(Some(store.latest_draw(spec).map_err(|e| e.to_string())?));
        }
    }
    draw_past(chain, fact, last, wait, Past::Latest)?;
    Ok(None)
}

/// [`ObservationResults::open_drawn`] on the registry's record `rec`, with a
/// chain error (the beacon's block hash read) retried per `wait`, and a
/// record that does not match the round (a stale or mixed read) read again
/// (`drawOf`, one block) before the next try.
#[allow(clippy::too_many_arguments)]
pub(super) fn open_retrying(
    store: &mut ObservationResults,
    spec: &ObservationSpec,
    cfg: ObserveConfig,
    snapshot: Vec<Observer>,
    chain: &dyn DrawChain,
    draw: &OnchainDraw,
    mut rec: DrawRecord,
    wait: &ChainWait,
) -> Result<ObservationRound, ObserveError> {
    let mut last = ObserveError::Chain("no try".into());
    for i in 0..wait.tries.max(1) {
        if i > 0 {
            std::thread::sleep(wait.pause);
        }
        match store.open_drawn(spec.clone(), cfg, snapshot.clone(), chain, &rec) {
            Err(ObserveError::Chain(e)) => last = ObserveError::Chain(e),
            Err(e @ ObserveError::RegistryDrawMismatch(_)) => {
                last = e;
                if let Ok(Some(r)) = chain.draw_of(&draw.fact, draw.attempt) {
                    if is_announced(&r, draw) && r.beacon != Hash32::ZERO {
                        rec = r;
                    }
                }
            }
            other => return other,
        }
    }
    Err(last)
}

/// What [`confirm_announced`] found for the book's announced draw.
pub(super) enum Confirm {
    /// The registry holds it (attempt, commitment, beacon height).
    Landed,
    /// A final block proves it can never land: release it.
    Gone(Release),
    /// Neither, after `wait`'s polls: the draw stays in the book.
    Unknown(String),
}

/// Whether the registry holds `draw`, the book's announced draw (reviews
/// R6-A, R6-B): [`Confirm::Landed`] when it does (`drawOf`, retried per
/// `wait`, at the latest block, read again on every poll so a landed draw's
/// beacon is fixed in time (review R7-1), or at a final block),
/// [`Confirm::Gone`] when ONE final block ([`DrawChain::draw_of_final`])
/// proves it never will: another draw at its attempt (`BadAttempt` for
/// ever), or none at or past its beacon height (`announceDraw` reverts
/// `BeaconNotFuture` from there on). [`Confirm::Unknown`] otherwise: the
/// caller keeps the draw in the book.
pub(super) fn confirm_announced(
    chain: &dyn DrawChain,
    draw: &OnchainDraw,
    wait: &ChainWait,
) -> Confirm {
    let mut last = String::from("no read");
    for i in 0..wait.polls.max(1) {
        if i > 0 {
            std::thread::sleep(wait.pause);
        }
        // Review R7-1: the latest block is read on EVERY poll, and a
        // matching draw there is acted on at once: its beacon must be fixed
        // within 256 blocks of its beacon height, and a final block can lag
        // the head by more (about 500 blocks on Base Sepolia). Only
        // forgetting the draw waits for a final block.
        if let Ok(Some(r)) = wait.retry(|| chain.draw_of(&draw.fact, draw.attempt)) {
            if is_announced(&r, draw) {
                return Confirm::Landed;
            }
        }
        match final_read(chain, draw, wait) {
            Ok(r) => match r.record {
                Some(x) if is_announced(&x, draw) => return Confirm::Landed,
                Some(_) => return Confirm::Gone(Release::NeverLands(r)),
                None if r.block >= draw.beacon_height => {
                    return Confirm::Gone(Release::NeverLands(r))
                }
                None => {
                    last = format!(
                        "no draw at attempt {} as of final block {}, below its beacon height {}",
                        draw.attempt, r.block, draw.beacon_height
                    );
                }
            },
            Err(e) => last = e,
        }
    }
    Confirm::Unknown(last)
}

/// The registry holds `draw` as announced: the same attempt, commitment and
/// beacon height.
pub(super) fn is_announced(r: &DrawRecord, draw: &OnchainDraw) -> bool {
    r.attempt == draw.attempt && r.commit == draw.commit && r.beacon_height == draw.beacon_height
}

/// ONE read of a draw at ONE final block ([`DrawChain::draw_of_final`]):
/// the block and what it shows at the attempt. The only material of a
/// chain release proof (review R8): both values come from the same block,
/// and a final block is never reorged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FinalRead {
    pub(super) block: u64,
    pub(super) record: Option<DrawRecord>,
}

/// Why the book's draw may leave it ([`ObservationResults::release_draw`],
/// the draw invariant, review R8). Private: a chain proof is built only from
/// a [`FinalRead`] here; [`Release::Published`] only by
/// [`ObservationResults::published`] and
/// [`ObservationResults::attempt_published`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Release {
    /// The round (settled here) or failed attempt with this id is durably
    /// written.
    Published(Hash32),
    /// The final block shows another draw at its attempt, or none at or past
    /// its beacon height.
    NeverLands(FinalRead),
    /// The final block, past its beacon height plus the `blockhash` window,
    /// shows it with a zero beacon.
    Unfixable(FinalRead),
    /// The save that would have held it failed, and nothing was sent.
    NeverSent,
}

/// `draw`'s attempt at one final block, retried per `wait`.
pub(super) fn final_read(
    chain: &dyn DrawChain,
    draw: &OnchainDraw,
    wait: &ChainWait,
) -> Result<FinalRead, String> {
    wait.retry(|| chain.draw_of_final(&draw.fact, draw.attempt))
        .map(|(block, record)| FinalRead { block, record })
}

/// A proof, from ONE final block, that the book's `draw` can never become a
/// fixed beacon without a published round: it can never land
/// ([`Release::NeverLands`]) or never have its beacon fixed
/// ([`Release::Unfixable`]). Err (why not) otherwise: the draw stays.
pub(super) fn gone_proof(
    chain: &dyn DrawChain,
    draw: &OnchainDraw,
    wait: &ChainWait,
) -> Result<Release, String> {
    let r = final_read(chain, draw, wait).map_err(|e| format!("no final read: {e}"))?;
    let window_end = draw.beacon_height.saturating_add(BLOCKHASH_WINDOW);
    match r.record {
        Some(x) if is_announced(&x, draw) => {
            if x.beacon != Hash32::ZERO {
                Err(format!(
                    "final block {} shows draw {} with its beacon fixed",
                    r.block, draw.attempt
                ))
            } else if r.block > window_end {
                Ok(Release::Unfixable(r))
            } else {
                Err(format!(
                    "final block {} shows draw {} with its beacon zero, still fixable until block {window_end}",
                    r.block, draw.attempt
                ))
            }
        }
        Some(_) => Ok(Release::NeverLands(r)),
        None if r.block >= draw.beacon_height => Ok(Release::NeverLands(r)),
        None => Err(format!(
            "final block {} is below draw {}'s beacon height {} and does not show it yet",
            r.block, draw.attempt, draw.beacon_height
        )),
    }
}

/// Which chain draw the book lacks [`draw_past`] judges.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Past {
    /// Below the chain's latest draw: it has a successor.
    Earlier,
    /// The chain's latest draw.
    Latest,
}

/// Review R7-2, R8-2: Ok when the chain's draw at `attempt`, one the book
/// lacks and cannot resume or adopt, may be drawn past, from ONE final
/// block ([`DrawChain::draw_of_final`]): its beacon zero past its beacon
/// height plus the 256-block `blockhash` window (`fixBeacon` reverts
/// `BeaconUnavailable` from there on), or, for an [`Past::Earlier`] draw,
/// its beacon fixed: a draw gets a successor only once it is released (its
/// round published), so a fixed earlier draw is settled history, never one
/// this book could still publish. Err (the run stops, nothing is announced)
/// otherwise: a zero, still fixable beacon; the latest draw with a fixed
/// beacon; or no final read.
pub(super) fn draw_past(
    chain: &dyn DrawChain,
    fact: &OnchainFact,
    attempt: u32,
    wait: &ChainWait,
    which: Past,
) -> Result<(), String> {
    let stop = |why: String| {
        format!(
            "the chain holds draw {attempt}, which the book cannot resume or adopt ({why}): restore the book this coordinator saved, or run with the inputs it announced (pool, spec, config); a draw the book lacks is drawn past only once its beacon can never be fixed (zero at a final block past its beacon height + {BLOCKHASH_WINDOW}), or when a later draw follows it with its beacon fixed"
        )
    };
    match wait.retry(|| chain.draw_of_final(fact, attempt)) {
        Ok((h, Some(r)))
            if r.beacon == Hash32::ZERO && h > r.beacon_height.saturating_add(BLOCKHASH_WINDOW) =>
        {
            Ok(())
        }
        Ok((_, Some(r))) if r.beacon != Hash32::ZERO && which == Past::Earlier => Ok(()),
        Ok((_, Some(r))) if r.beacon != Hash32::ZERO => Err(stop("its beacon is fixed".into())),
        Ok((h, Some(r))) => Err(stop(format!(
            "its beacon is zero at final block {h}, still fixable until block {}",
            r.beacon_height.saturating_add(BLOCKHASH_WINDOW)
        ))),
        Ok((h, None)) => Err(stop(format!("final block {h} does not show it yet"))),
        Err(e) => Err(stop(format!("no final read: {e}"))),
    }
}

/// Fix `draw`'s beacon, idempotently (review REVLOG-R5; L2RND-R2-F4):
/// `fixBeacon` is public and fixes a beacon once, so the registry is read
/// first ([`DrawChain::draw_of`], retried per `wait`, review R8-3: a stale
/// read is retried, not taken as the answer) and a beacon already fixed is
/// success whoever fixed it; after the send, and after ANY send error (a
/// stranger's `fixBeacon` landing first makes it revert
/// `BeaconAlreadyFixed`, or a read after the coordinator's own send failed),
/// the draw is read again, with `wait`'s retries, and a fixed beacon on the
/// announced draw (attempt, commitment, beacon height) is success. Err only
/// when the chain shows the beacon unfixed (or cannot be read) after the
/// retries.
pub fn fix_beacon_idempotent(
    chain: &mut dyn DrawChain,
    draw: &OnchainDraw,
    wait: &ChainWait,
) -> Result<DrawRecord, String> {
    let fixed = |r: Option<DrawRecord>| -> Result<Option<DrawRecord>, String> {
        match r {
            None => Err(format!("drawOf: no draw at attempt {}", draw.attempt)),
            Some(r) if !is_announced(&r, draw) => Err(format!(
                "drawOf: the registry's draw at attempt {} is not the one announced",
                draw.attempt
            )),
            Some(r) => Ok((r.beacon != Hash32::ZERO).then_some(r)),
        }
    };
    if let Some(r) = wait.retry(|| fixed(chain.draw_of(&draw.fact, draw.attempt)?))? {
        return Ok(r);
    }
    let sent = match chain.fix_beacon(draw) {
        Ok(r) if is_announced(&r, draw) && r.beacon != Hash32::ZERO => return Ok(r),
        Ok(_) => "fixBeacon read back another draw".to_string(),
        Err(e) => e,
    };
    wait.retry(|| {
        fixed(chain.draw_of(&draw.fact, draw.attempt)?)?
            .ok_or_else(|| format!("the beacon is not fixed on chain ({sent})"))
    })
}

/// `fix_beacon_idempotent` failed with `e`: the announced draw stays in the
/// book (saved), to be resumed by the next run on the same draw (review
/// REVLOG-R5), unless ONE final block proves it can never become a fixed
/// beacon without a published round ([`gone_proof`], review R6-D): its
/// beacon block left the chain's `blockhash` window with the beacon unfixed
/// (no committee exists for it, clients accept a draw without a beacon), or
/// it can never land. Then it is released
/// ([`ObservationResults::release_draw`]).
pub(super) fn beacon_not_fixed(
    store: &mut ObservationResults,
    spec: &ObservationSpec,
    chain: &dyn DrawChain,
    draw: &OnchainDraw,
    wait: &ChainWait,
    save: &mut dyn FnMut(&ObservationResults) -> Result<(), String>,
    e: String,
) -> String {
    match gone_proof(chain, draw, wait) {
        Ok(proof) => match release(store, spec, proof, save) {
            Ok(()) => format!("fix beacon: {e}; draw {} can never have its beacon fixed (a final block shows it unfixable or never landed): it is given up (no beacon, no committee); the next run draws again", draw.attempt),
            Err(a) => format!("fix beacon: {e}; giving the draw up failed: {a}; it stays in the book"),
        },
        Err(_) => format!(
            "fix beacon: {e}; draw {} stays announced in the book: run again with the same book to fix its beacon and open it on the same draw",
            draw.attempt
        ),
    }
}

/// The public inputs of a round's observer draw (L2-F2, ADR-0027 amendment 6).
/// The coordinator never picks observers: they are the `size` pool members
/// with the smallest draw keys, where every key hashes the feed, the pool
/// snapshot and a beacon announced before it existed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DrawSeed {
    /// [`pool_hash`] of the snapshot the observers are drawn from.
    pub pool: Hash32,
    /// The beacon's height, announced before that block existed
    /// ([`ObservationResults::announce_draw`]).
    pub beacon_height: u64,
    /// The beacon: the deployment chain's block hash at `beacon_height`.
    pub beacon: Hash32,
    /// 0 for a fact's first draw, +1 for each draw of the fact that ended
    /// without a value (a fresh announcement and beacon each time).
    pub attempt: u32,
}

impl DrawSeed {
    /// The seed of a draw from `pool` with `beacon` at `beacon_height`.
    pub fn for_pool(pool: &[Observer], beacon_height: u64, beacon: Hash32, attempt: u32) -> Self {
        DrawSeed {
            pool: pool_hash(pool),
            beacon_height,
            beacon,
            attempt,
        }
    }
}

/// `keccak256("onyx-observer-pool-v1" ‖ borsh(pool))`.
pub fn pool_hash(pool: &[Observer]) -> Hash32 {
    let mut b = b"onyx-observer-pool-v1".to_vec();
    b.extend_from_slice(&borsh::to_vec(pool).unwrap_or_default());
    keccak256(&b)
}

/// `keccak256("onyx-observe-cfg-v1" ‖ borsh(cfg))`: the whole round config,
/// so `outlier_slash` (the draw's `min_bond`) is fixed before the beacon
/// (L2RND-R3-F2).
pub fn cfg_hash(cfg: &ObserveConfig) -> Hash32 {
    let mut b = b"onyx-observe-cfg-v1".to_vec();
    b.extend_from_slice(&borsh::to_vec(cfg).unwrap_or_default());
    keccak256(&b)
}

/// `keccak256("onyx-draw-commit-v1" ‖ pool ‖ spec_id ‖ cfg_hash)`, the
/// packing of `OnyxRegistry.drawCommit` (ADR-0011 style). `announceDraw`
/// stores it before the beacon exists and `recordObservation` refuses any
/// other (pool, spec, config): one announced draw reaches one committee
/// (L2RND-R3-F1/F2). The spec id covers the window, which a dated fact key
/// does not.
pub fn draw_commit(pool: &Hash32, spec_id: &Hash32, cfg_hash: &Hash32) -> Hash32 {
    let mut b = b"onyx-draw-commit-v1".to_vec();
    b.extend_from_slice(&pool.0);
    b.extend_from_slice(&spec_id.0);
    b.extend_from_slice(&cfg_hash.0);
    keccak256(&b)
}

/// `keccak256("onyx-observe-draw-v1" ‖ borsh(seed))`.
pub fn draw_hash(seed: &DrawSeed) -> Hash32 {
    let mut b = b"onyx-observe-draw-v1".to_vec();
    b.extend_from_slice(&borsh::to_vec(seed).unwrap_or_default());
    keccak256(&b)
}

/// A pool snapshot a draw may use: 1..=[`MAX_POOL`] members in strictly
/// ascending (so distinct) address order.
pub fn check_pool(pool: &[Observer]) -> Result<(), ObserveError> {
    if pool.is_empty()
        || pool.len() > MAX_POOL
        || !pool.windows(2).all(|w| w[0].address < w[1].address)
    {
        return Err(ObserveError::BadPool);
    }
    Ok(())
}

/// The observers of a round (pure, public: every consumer recomputes it).
/// Members whose snapshot bond covers `min_bond` are eligible; each gets the
/// key `keccak256("onyx-observe-draw-v1" ‖ spec_id ‖ draw_hash(seed) ‖
/// address)`; the `size` smallest keys are drawn and returned ascending by
/// address. Empty when fewer than `size` members are eligible.
pub fn draw_observers(
    spec_id: &Hash32,
    pool: &[Observer],
    seed: &DrawSeed,
    size: usize,
    min_bond: u128,
) -> Vec<Observer> {
    let dh = draw_hash(seed);
    let mut keyed: Vec<(Hash32, Observer)> = pool
        .iter()
        .filter(|o| o.bond >= min_bond)
        .map(|o| {
            let mut b = b"onyx-observe-draw-v1".to_vec();
            b.extend_from_slice(&spec_id.0);
            b.extend_from_slice(&dh.0);
            b.extend_from_slice(&o.address);
            (keccak256(&b), *o)
        })
        .collect();
    if size == 0 || keyed.len() < size {
        return Vec::new();
    }
    keyed.sort_unstable_by_key(|a| a.0);
    let mut drawn: Vec<Observer> = keyed.into_iter().take(size).map(|(_, o)| o).collect();
    drawn.sort_by_key(|o| o.address);
    drawn
}

/// Every check a consumer applies to a served round's draw: the pool is a
/// valid snapshot named by `seed.pool`, the attempt is within
/// [`MAX_DRAW_ATTEMPTS`], and `observers` are exactly the draw of
/// `spec.min_observers` members. The beacon itself is the coordinator's word
/// unless the consumer compares it with the chain's block hash at
/// `seed.beacon_height` (THREATS L2-F2).
pub fn check_draw(
    spec: &ObservationSpec,
    cfg: &ObserveConfig,
    pool: &[Observer],
    seed: &DrawSeed,
    observers: &[Observer],
) -> Result<(), ObserveError> {
    check_pool(pool)?;
    if pool_hash(pool) != seed.pool || seed.attempt >= MAX_DRAW_ATTEMPTS {
        return Err(ObserveError::BadPool);
    }
    let drawn = draw_observers(
        &spec.id(),
        pool,
        seed,
        usize::from(spec.min_observers),
        cfg.outlier_slash,
    );
    if drawn.is_empty() {
        return Err(ObserveError::TooFewObservers {
            have: pool.iter().filter(|o| o.bond >= cfg.outlier_slash).count(),
            need: spec.min_observers,
        });
    }
    if drawn != observers {
        return Err(ObserveError::NotTheDraw);
    }
    Ok(())
}

/// Check a draw's beacon against the chain (L2RND-R1-F1): it must be the
/// block hash at `seed.beacon_height`. A consumer runs this with its own
/// chain reader, never the coordinator's; on chain, `OnyxRegistry.drawMatches`
/// checks the same against the beacon `fixBeacon` read with `blockhash`.
pub fn check_beacon(seed: &DrawSeed, chain: &dyn ChainView) -> Result<(), ObserveError> {
    match chain
        .block_hash(seed.beacon_height)
        .map_err(ObserveError::Chain)?
    {
        None => Err(ObserveError::BeaconNotYet),
        Some(h) if h == seed.beacon => Ok(()),
        Some(_) => Err(ObserveError::BeaconMismatch),
    }
}

/// The rule an observer applies before it commits (ATT-R1-F4): the
/// registry's `drawOf(fact, attempt)`, read by the observer from its own
/// chain reader, must show this round's draw (`draw_commit`, beacon height,
/// announced before it) with its beacon FIXED and equal to the round's.
/// A client reads a draw on chain without a fixed beacon as one that never
/// had a committee (`client::verify_attempts_onchain`); that holds only
/// because honest observers refuse to commit to such a draw. Without this
/// rule a coordinator could compute the committee from the block hash, run
/// the round, and never call `fixBeacon` when it disliked the outcome.
pub fn check_commit_allowed(
    seed: &DrawSeed,
    draw_commit: &Hash32,
    draw_of: &DrawRecord,
) -> Result<(), ObserveError> {
    if draw_of.beacon == Hash32::ZERO {
        return Err(ObserveError::BeaconNotFixed);
    }
    if draw_of.attempt != seed.attempt
        || draw_of.commit != *draw_commit
        || draw_of.beacon_height != seed.beacon_height
        || draw_of.announced_at >= draw_of.beacon_height
        || draw_of.beacon != seed.beacon
    {
        return Err(ObserveError::BeaconMismatch);
    }
    Ok(())
}
