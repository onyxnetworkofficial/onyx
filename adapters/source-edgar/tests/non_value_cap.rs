//! SEC-R2-F1: a period fact stays open after a round that settles no value
//! (SEC-R1-F1), but only up to `MAX_NON_VALUE_ROUNDS` such rounds; then it is
//! closed until a reviewer reopens it.

use onyx_coordinator::observe::NoInbox;
use onyx_coordinator::observe::{fact_key_dated, MAX_NON_VALUE_ROUNDS};
use onyx_coordinator::observe::{ChainWait, ReplayClock, SimulatedRegistry};
use onyx_source_edgar::round::{
    load_book, run, run_and_save, run_waiting, save_book, store, store_with_admin, Page,
};
use onyx_source_edgar::{recorded, spec};

fn forged(body: &[u8]) -> Vec<u8> {
    String::from_utf8(body.to_vec())
        .expect("utf8")
        .replace("\"val\":762099", "\"val\":900000")
        .into_bytes()
}

fn pages(t: u64, bodies: [Vec<u8>; 3]) -> Vec<Page> {
    bodies
        .into_iter()
        .enumerate()
        .map(|(i, b)| Page {
            fetched_unix: t + i as u64,
            body: Some(b),
        })
        .collect()
}

/// The residual the cap bounds (review SEC-R2, documented in THREATS SEC-R1-F1
/// and L2-F2): a split round (honest V, colluder W, one blocked) settles
/// NoResult and leaves the period open; a later round whose drawn majority
/// says W settles W at level 2. Each later round is a fresh draw with a fresh
/// beacon (L2-F2), and the cap bounds how often it can be tried.
#[test]
fn a_split_round_leaves_the_period_open_and_a_later_majority_settles() {
    let mut chain = SimulatedRegistry::new();
    let mut s = store();
    let rec = &recorded()[0];
    let honest = rec.fetches[0].body.clone();
    let bad = forged(&honest);
    assert_ne!(bad, honest);
    let t = rec.fetches[0].fetched_unix;
    let first = run(
        &rec.company,
        &rec.period_end,
        rec.window_start,
        &pages(t, [honest.clone(), bad.clone(), b"403".to_vec()]),
        &mut s,
        &mut chain,
    );
    assert!(first
        .expect("runs")
        .value()
        .expect_err("no value")
        .contains("NoResult"));
    let second = run(
        &rec.company,
        &rec.period_end,
        rec.window_start + 86_400,
        &pages(t + 86_400, [bad.clone(), bad, honest]),
        &mut s,
        &mut chain,
    )
    .expect("the period is still open");
    assert_eq!(
        second.value().expect("value").value,
        onyx_core::Value::Int(900_000)
    );
}

/// After `MAX_NON_VALUE_ROUNDS` rounds without a value the period is closed:
/// neither opening nor settling another round works until the admin reopens it
/// (recorded), and then the honest value settles.
#[test]
fn the_cap_closes_a_period_until_a_reviewer_reopens_it() {
    let mut chain = SimulatedRegistry::new();
    let (mut s, admin) = store_with_admin();
    let rec = &recorded()[0];
    let honest = rec.fetches[0].body.clone();
    let bad = forged(&honest);
    let t = rec.fetches[0].fetched_unix;
    let fact =
        fact_key_dated(&spec(&rec.company, &rec.period_end, rec.window_start).expect("spec"));
    for k in 0..u64::from(MAX_NON_VALUE_ROUNDS) {
        let shift = k * 86_400;
        let r = run(
            &rec.company,
            &rec.period_end,
            rec.window_start + shift,
            &pages(t + shift, [honest.clone(), bad.clone(), b"403".to_vec()]),
            &mut s,
            &mut chain,
        );
        assert!(r
            .expect("runs")
            .value()
            .expect_err("split")
            .contains("NoResult"));
    }
    assert_eq!(s.fact_state(&fact), (true, MAX_NON_VALUE_ROUNDS));
    let shift = u64::from(MAX_NON_VALUE_ROUNDS) * 86_400;
    let closed = run(
        &rec.company,
        &rec.period_end,
        rec.window_start + shift,
        &pages(t + shift, [honest.clone(), honest.clone(), honest.clone()]),
        &mut s,
        &mut chain,
    );
    assert!(closed
        .expect_err("closed")
        .error
        .contains("reviewer must reopen"));
    s.reopen_fact(&admin, fact, "source restored; reviewed", 1)
        .expect("reopen");
    // The registry owner records the matching on-chain reopen (L2RND-R2-F1).
    chain.reopen_draws(
        &s.onchain_fact(&spec(&rec.company, &rec.period_end, rec.window_start).expect("spec"))
            .expect("fact"),
    );
    assert_eq!(s.fact_decisions().len(), 1);
    assert_eq!(s.fact_state(&fact), (false, 0));
    let ok = run(
        &rec.company,
        &rec.period_end,
        rec.window_start + shift,
        &pages(t + shift, [honest.clone(), honest.clone(), honest]),
        &mut s,
        &mut chain,
    )
    .expect("reopened");
    assert_eq!(
        ok.value().expect("value").value,
        onyx_core::Value::Int(762_099)
    );
    // L2-F2: the reopened round is a new draw; reopening never resets the
    // attempt, so no earlier announcement (whose beacon is known) is reused.
    assert_eq!(ok.draw.attempt, MAX_NON_VALUE_ROUNDS);
    // The value closed the period for good.
    assert!(s.reopen_fact(&admin, fact, "again", 2).is_err());
}

/// The cap is a reviewer setting, recorded, and never below 1.
#[test]
fn the_cap_is_a_recorded_reviewer_setting() {
    let (mut s, admin) = store_with_admin();
    s.set_non_value_cap(&admin, 0, "strict", 5).expect("set");
    assert_eq!(s.fact_decisions()[0].cap, 1);
    let (_s2, admin2) = store_with_admin();
    assert!(s.set_non_value_cap(&admin2, 2, "foreign", 6).is_err());
}

/// AUD-2-F9: the runner keeps the non-value book across a restart. Three
/// split rounds, a restart (a fresh store that reads the book), and the
/// fourth round is still refused; a corrupt book is an error, not a reset.
#[test]
fn a_restart_keeps_the_cap() {
    let mut chain = SimulatedRegistry::new();
    let dir = std::env::temp_dir().join(format!("onyx-aud2-f9-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("book.json").to_string_lossy().into_owned();
    let _ = std::fs::remove_file(&path);
    let rec = &recorded()[0];
    let honest = rec.fetches[0].body.clone();
    let bad = forged(&honest);
    let t = rec.fetches[0].fetched_unix;
    let fact =
        fact_key_dated(&spec(&rec.company, &rec.period_end, rec.window_start).expect("spec"));
    for k in 0..u64::from(MAX_NON_VALUE_ROUNDS) {
        // One process per round: read the book, run, write it.
        let mut s = store();
        load_book(&mut s, &path).expect("load");
        let shift = k * 86_400;
        let r = run(
            &rec.company,
            &rec.period_end,
            rec.window_start + shift,
            &pages(t + shift, [honest.clone(), bad.clone(), b"403".to_vec()]),
            &mut s,
            &mut chain,
        );
        assert!(r
            .expect("runs")
            .value()
            .expect_err("split")
            .contains("NoResult"));
        save_book(&s, &path).expect("save");
    }
    let mut s = store();
    load_book(&mut s, &path).expect("load");
    assert_eq!(s.fact_state(&fact), (true, MAX_NON_VALUE_ROUNDS));
    let shift = u64::from(MAX_NON_VALUE_ROUNDS) * 86_400;
    let closed = run(
        &rec.company,
        &rec.period_end,
        rec.window_start + shift,
        &pages(t + shift, [honest.clone(), honest.clone(), honest]),
        &mut s,
        &mut chain,
    );
    assert!(closed
        .expect_err("closed")
        .error
        .contains("reviewer must reopen"));
    std::fs::write(&path, "{\"not\": \"a book\"}").expect("write");
    assert!(load_book(&mut store(), &path).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// AUD-2R-F4: the book is written after every round, including a round that
/// settles no value (an error the runner propagates), and the write is
/// atomic: a "kill" right after that round (the process never reaches its
/// final save) keeps the count; a torn book stops the start instead of
/// resetting the cap; the reviewer's cap and decisions survive a restart.
#[test]
fn every_round_is_saved_and_a_torn_book_refuses_the_start() {
    let mut chain = SimulatedRegistry::new();
    let dir = std::env::temp_dir().join(format!("onyx-aud2r-f4-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("book.json").to_string_lossy().into_owned();
    let _ = std::fs::remove_file(&path);
    let rec = &recorded()[0];
    let honest = rec.fetches[0].body.clone();
    let bad = forged(&honest);
    let t = rec.fetches[0].fetched_unix;
    let fact =
        fact_key_dated(&spec(&rec.company, &rec.period_end, rec.window_start).expect("spec"));
    {
        let (mut s, admin) = store_with_admin();
        load_book(&mut s, &path).expect("first run");
        s.set_non_value_cap(&admin, 5, "reviewer raised the cap", 7)
            .expect("cap");
        // A round without a value: run_and_save returns its error but the
        // book is already on disk. The process then "dies" (scope ends).
        let r = run_and_save(
            &rec.company,
            &rec.period_end,
            rec.window_start,
            &pages(t, [honest.clone(), bad.clone(), b"403".to_vec()]),
            &mut s,
            &mut chain,
            &path,
            None,
            &ChainWait::NOW,
            &ReplayClock::default(),
            &mut NoInbox,
            &mut |_| Ok(()),
            &mut |_| Ok(()),
        );
        assert!(r
            .expect("runs")
            .value()
            .expect_err("split")
            .contains("NoResult"));
    }
    assert!(
        !dir.join(".book.json.tmp").exists(),
        "no temporary file left behind"
    );
    let mut s = store();
    load_book(&mut s, &path).expect("restart");
    assert_eq!(s.fact_state(&fact), (false, 1), "the count survived");
    assert_eq!(
        s.non_value_book().expect("writable")["cap"],
        5,
        "the reviewer's cap survived"
    );
    assert_eq!(s.fact_decisions().len(), 1, "the decision survived");
    // A torn book (half written) is refused, not read as a first run.
    let full = std::fs::read_to_string(&path).expect("book");
    std::fs::write(&path, &full[..full.len() / 2]).expect("tear");
    let err = load_book(&mut store(), &path).expect_err("torn");
    assert!(err.contains("refusing to start"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review REVLOG-R4-2: a reviewer lowers the cap while a fact has an
/// unfinished round. In the process that is fine (the fact closes at its next
/// round without a value), but loading the saved book closed the fact by its
/// count BEFORE restoring the round, so `load_book` failed (`FactClosed`) and
/// the coordinator could not start. Now the round is restored first and a
/// fact with one is not closed by its count; the round then finishes on its
/// own draw.
#[test]
fn lowering_the_cap_while_a_round_is_unfinished_still_loads() {
    let mut chain = SimulatedRegistry::new();
    let rec = &recorded()[0];
    let honest = rec.fetches[0].body.clone();
    let t = rec.fetches[0].fetched_unix;
    let sp = spec(&rec.company, &rec.period_end, rec.window_start).expect("spec");
    let fact = fact_key_dated(&sp);
    let dir = std::env::temp_dir().join(format!("onyx-r4-2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("book.json").to_string_lossy().into_owned();
    let _ = std::fs::remove_file(&path);
    let (mut s, admin) = store_with_admin();
    // One round without a value (split), then a round whose log cannot be
    // read yet (the chain's finalized block is not past commit close): it
    // stays unfinished on draw 1.
    let split = run_waiting(
        &rec.company,
        &rec.period_end,
        rec.window_start,
        &pages(t, [honest.clone(), forged(&honest), b"403".to_vec()]),
        &mut s,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect("runs");
    assert!(split.value().is_err());
    chain.time = 0;
    let honest_pages = pages(t, [honest.clone(), honest.clone(), honest.clone()]);
    let e = run_waiting(
        &rec.company,
        &rec.period_end,
        rec.window_start,
        &honest_pages,
        &mut s,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect_err("the commit phase is not final yet");
    assert!(e.unfinished.is_some());
    // The reviewer lowers the cap to the fact's count.
    s.set_non_value_cap(&admin, 1, "reviewer lowered the cap", 9)
        .expect("cap");
    save_book(&s, &path).expect("saved");
    let mut fresh = store();
    load_book(&mut fresh, &path).expect("the book loads");
    assert!(fresh.unfinished(&sp).is_some(), "the round is back");
    assert_eq!(
        fresh.fact_state(&fact),
        (false, 1),
        "not closed by its count"
    );
    // The round finishes on its own draw.
    chain.time = u64::MAX;
    let r = run_waiting(
        &rec.company,
        &rec.period_end,
        rec.window_start,
        &honest_pages,
        &mut fresh,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect("finished");
    assert_eq!(r.draw.attempt, 1, "the same draw");
    r.value().expect("the value");
    let _ = std::fs::remove_dir_all(&dir);
}
