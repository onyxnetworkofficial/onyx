//! Chained logs in pages and the registry served for reruns (ROADMAP C21,
//! C23) against a lying server: rows changed, cut, shifted or from another
//! run; a registry that does not hash to the commitment's `registry_root`.

use super::*;
use crate::example::{self, AS_OF, AS_OF_EARLY, FORECASTER, GUESSER, METRIC, SIGNER, T0};
use crate::publication::PublicationError;
use crate::wire::{check_log_body, log_body, MAX_LOG_ROWS};
use crate::{compute, sort_resolutions, sort_statements, Fact, Resolution, Statement};
use onyx_core::{
    keccak256, Confidence, MetricId, NoRules, ProofLevel, Registry, SourceId, Subject, Value,
};

const DAY: u64 = 86_400;

/// The example chain as chained logs, with the registry.
fn example_chained() -> Publication {
    let (p, reg) = (example::params(), example::registry());
    let mut out = Publication::chained();
    for as_of in [AS_OF_EARLY, AS_OF] {
        let (s, r) = example::logs(as_of);
        let book = compute(&p, &reg, &s, &r, as_of).expect("compute");
        out.push_with(&p, &s, &r, &book, None, Some(&reg))
            .expect("push");
    }
    out
}

/// A large example-like log: `days` facts, each stated by both sources and
/// answered by the signer, cut at `as_of`.
fn big_logs(days: u64, as_of: u64) -> (Vec<Statement>, Vec<Resolution>) {
    let (mut s, mut r) = (Vec::new(), Vec::new());
    for d in 0..days {
        let f = Fact {
            metric: MetricId(METRIC.into()),
            subject: Subject::Entity(example::ENTITIES[(d % 6) as usize].into()),
            as_of: T0 + d * 60,
        };
        for (src, v) in [(FORECASTER, 1u128), (GUESSER, u128::from(d % 2))] {
            s.push(Statement {
                source: SourceId(src.into()),
                fact: f.clone(),
                value: Value::UInt(v),
                confidence: Confidence(900_000),
                committed_at: f.as_of - 30,
            });
        }
        r.push(Resolution {
            fact: f.clone(),
            value: Value::UInt(1),
            level: ProofLevel::SourceProven,
            resolver: SourceId(SIGNER.into()),
            evidence: keccak256(&d.to_be_bytes()),
            public_from: f.as_of + 30,
        });
    }
    s.retain(|x| x.committed_at <= as_of);
    r.retain(|x| x.public_from <= as_of);
    sort_statements(&mut s);
    sort_resolutions(&mut r);
    (s, r)
}

/// Fetch every page of `p` from the run at step `from` into a [`PagedLog`].
fn page_through(
    p: &Publication,
    from: usize,
    expect: &LogExpect<'_>,
) -> Result<CheckedLog, LogError> {
    let head = chained_log_body(p, from).expect("head");
    let mut log = PagedLog::start(&head, expect)?;
    let first = p
        .steps
        .iter()
        .position(|s| s.commitment.hash() == log.since());
    while let Some(pos) = log.next_rows() {
        let page = log_rows_page(p, first.expect("run"), pos).expect("page");
        log.take_rows(&page)?;
    }
    while let Some(n) = log.next_posts() {
        log.take_posts(&log_posts_page(p, n).expect("posts"))?;
    }
    log.finish(expect)
}

/// ROADMAP C21/C23: the example as chained logs, paged, with the served
/// registry checked to `registry_root` and used for the rerun.
#[test]
fn chained_logs_are_checked_page_by_page_and_rerun_with_the_served_registry() {
    let p = example_chained();
    let c1 = p.steps[0].commitment.clone();
    let c2 = p.steps[1].commitment.clone();
    let reg = check_registry_body(&registry_body(&p).expect("registry"), &c2, &NoRules)
        .expect("registry checks");
    assert_eq!(reg.root(), example::registry().root());
    let e = LogExpect {
        known: Some(c1.hash()),
        latest: Some(c2.hash()),
        min_as_of: AS_OF,
        registry: Some(&reg),
        ..Default::default()
    };
    let got = page_through(&p, 0, &e).expect("checks");
    assert!(got.recomputed && got.example && !got.has_intake);
    assert_eq!(got.links, 2);
    assert_eq!((got.statements, got.resolutions), example::logs(AS_OF));
    // Without `since`: the latest link alone.
    let e1 = LogExpect {
        registry: Some(&reg),
        ..Default::default()
    };
    assert_eq!(page_through(&p, 1, &e1).expect("checks").links, 1);
    // A chained publication has no format-1 body, and a format-1 client
    // refuses the head.
    assert!(log_body(&p, 0).is_none());
    let head = chained_log_body(&p, 0).expect("head");
    assert!(matches!(
        check_log_body(&head, &LogExpect::default()),
        Err(LogError::Malformed(_))
    ));
}

/// TR-C20-5 closed: a log past 16,384 rows is served in pages of 4,096 and
/// rerun; a format-1 body over the same rows is refused by the client.
#[test]
fn a_log_past_the_body_cap_is_served_in_pages_and_rerun() {
    let (p, reg) = (example::params(), example::registry());
    let days = 7_000;
    let (t1, t2) = (T0 + 3_000 * 60, T0 + days * 60 + DAY);
    let mut pubn = Publication::chained();
    for as_of in [t1, t2] {
        let (s, r) = big_logs(days, as_of);
        let book = compute(&p, &reg, &s, &r, as_of).expect("compute");
        pubn.push_with(&p, &s, &r, &book, None, Some(&reg))
            .expect("push");
    }
    let rows = pubn.statements.len() + pubn.resolutions.len();
    assert!(rows as u64 > MAX_LOG_ROWS, "{rows} rows");
    pubn.check().expect("node check");
    let e = LogExpect {
        known: Some(pubn.steps[0].commitment.hash()),
        registry: Some(&reg),
        ..Default::default()
    };
    let got = page_through(&pubn, 0, &e).expect("checks");
    assert!(got.recomputed);
    assert_eq!(got.statements.len() + got.resolutions.len(), rows);
    // The same rows in a format-1 body: past the client's row cap.
    let mut whole = Publication::new();
    let (s, r) = big_logs(days, t2);
    let book = compute(&p, &reg, &s, &r, t2).expect("compute");
    whole.push(&p, &s, &r, &book, None).expect("push");
    assert_eq!(
        check_log_body(&log_body(&whole, 0).expect("body"), &LogExpect::default()).err(),
        Some(LogError::TooLarge)
    );
}

/// A page shifted, cut, from another run or another commitment, with a row
/// changed or of the wrong kind: refused at that page.
#[test]
fn a_tampered_page_is_refused() {
    let (p, reg) = (example::params(), example::registry());
    let days = 3_000;
    let mut pubn = Publication::chained();
    for as_of in [T0 + 1_000 * 60, T0 + days * 60 + DAY] {
        let (s, r) = big_logs(days, as_of);
        let book = compute(&p, &reg, &s, &r, as_of).expect("compute");
        pubn.push(&p, &s, &r, &book, None).expect("push");
    }
    let e = LogExpect::default();
    let start = || PagedLog::start(&chained_log_body(&pubn, 0).expect("head"), &e).expect("start");
    let page = |pos| log_rows_page(&pubn, 0, pos).expect("page");
    // Shifted: the second page served first.
    assert_eq!(
        start().take_rows(&page(LOG_PAGE_ROWS)).err(),
        Some(LogError::Malformed("from"))
    );
    // Another run (since the latest): its `since` differs.
    assert_eq!(
        start()
            .take_rows(&log_rows_page(&pubn, 1, 0).expect("page"))
            .err(),
        Some(LogError::Commitment)
    );
    // Cut.
    let mut cut = page(0);
    let mut rows: Vec<LogRow> = borsh::from_slice(
        &hex::decode(cut["rows"].as_str().expect("hex").trim_start_matches("0x")).expect("hex"),
    )
    .expect("rows");
    let last = rows.pop().expect("row");
    cut["rows"] = json!(super::super::borsh_hex(&rows));
    assert_eq!(
        start().take_rows(&cut).err(),
        Some(LogError::Malformed("rows"))
    );
    // A changed row: the first link's hashes fail where its rows end.
    rows.push(last);
    if let Some(LogRow::Statement(s)) = rows.get_mut(0) {
        s.confidence = Confidence(950_000);
    }
    let mut changed = page(0);
    changed["rows"] = json!(super::super::borsh_hex(&rows));
    let mut log = start();
    let mut res = log.take_rows(&changed);
    while res.is_ok() {
        match log.next_rows() {
            Some(pos) => res = log.take_rows(&page(pos)),
            None => break,
        }
    }
    assert_eq!(res.err(), Some(LogError::Steps(PublicationError::Log(0))));
    // A resolution where a statement is due.
    let mut kinds = page(0);
    let mut wrong = rows.clone();
    wrong[0] = LogRow::Resolution(pubn.resolutions[0].clone());
    kinds["rows"] = json!(super::super::borsh_hex(&wrong));
    assert_eq!(
        start().take_rows(&kinds).err(),
        Some(LogError::Steps(PublicationError::Counts(0)))
    );
    // Another latest commitment named by the page.
    let mut other = page(0);
    other["commitmentHash"] = json!(Hash32([1; 32]).to_string());
    assert_eq!(start().take_rows(&other).err(), Some(LogError::Commitment));
    // Finished early.
    let log = start();
    assert!(log.finish(&e).is_err());
}

/// The head: the latest named, the floor, `known`, the row count, the
/// format, and the client's budget before any row is fetched.
#[test]
fn a_head_past_the_client_limits_or_misnamed_is_refused() {
    let p = example_chained();
    let head = chained_log_body(&p, 0).expect("head");
    let e = LogExpect::default();
    let mut rows = head.clone();
    rows["rows"] = json!(1);
    assert_eq!(
        PagedLog::start(&rows, &e).err(),
        Some(LogError::Malformed("rows"))
    );
    let mut fmt = head.clone();
    fmt["format"] = json!(1);
    assert_eq!(
        PagedLog::start(&fmt, &e).err(),
        Some(LogError::Malformed("format"))
    );
    // Another page size (review C21-R2: no test changed it): refused, so a
    // server cannot make pages smaller to multiply its client's asks.
    for n in [1, 4_095, 4_097] {
        let mut size = head.clone();
        size["pageRows"] = json!(n);
        assert_eq!(
            PagedLog::start(&size, &e).err(),
            Some(LogError::Malformed("pageRows"))
        );
    }
    assert_eq!(
        PagedLog::start(
            &head,
            &LogExpect {
                known: Some(Hash32([9; 32])),
                ..Default::default()
            }
        )
        .err(),
        Some(LogError::NotDescendant)
    );
    assert_eq!(
        PagedLog::start(
            &head,
            &LogExpect {
                latest: Some(p.steps[0].commitment.hash()),
                ..Default::default()
            }
        )
        .err(),
        Some(LogError::NotLatest)
    );
    // A link claiming more rows than the budget: refused before any page.
    let mut huge = p.clone();
    huge.steps[1].statements = MAX_LOG_CHECK;
    let b = chained_log_body(&huge, 1).expect("head");
    assert_eq!(PagedLog::start(&b, &e).err(), Some(LogError::TooLarge));
}

/// ROADMAP C23: a registry that does not hash to the commitment's
/// `registry_root` (an entry added, another root named, another commitment)
/// is refused, never used for the rerun; one needing rules the client lacks
/// is refused too.
#[test]
fn a_forged_registry_is_refused() {
    let p = example_chained();
    let c = p.steps[1].commitment.clone();
    let body = registry_body(&p).expect("registry");
    let mut more = p.clone();
    if let Some(r) = more.registry.as_mut() {
        r.aggregates.push("example.other".into());
    }
    assert!(matches!(
        check_registry_body(&registry_body(&more).expect("body"), &c, &NoRules),
        Err(LogError::Registry(
            onyx_core::RegistryDataError::Root { .. }
        ))
    ));
    let mut root = body.clone();
    root["registryRoot"] = json!(Hash32([2; 32]).to_string());
    assert_eq!(
        check_registry_body(&root, &c, &NoRules).err(),
        Some(LogError::RegistryDiffers)
    );
    assert_eq!(
        check_registry_body(&body, &p.steps[0].commitment, &NoRules).err(),
        Some(LogError::Commitment)
    );
    let mut extra = body.clone();
    extra["values"] = json!([]);
    assert!(matches!(
        check_registry_body(&extra, &c, &NoRules),
        Err(LogError::Malformed(_))
    ));
    // A push whose registry is not the book's is refused at the source.
    let (pp, reg) = (example::params(), example::registry());
    let (s, r) = example::logs(AS_OF_EARLY);
    let book = compute(&pp, &reg, &s, &r, AS_OF_EARLY).expect("compute");
    let mut other = example::registry();
    other.add_aggregate("example.other").expect("aggregate");
    assert_eq!(
        Publication::chained()
            .push_with(&pp, &s, &r, &book, None, Some(&other))
            .err(),
        Some(PublicationError::Registry)
    );
    // Version 1 carries no registry.
    assert_eq!(
        Publication::new()
            .push_with(&pp, &s, &r, &book, None, Some(&reg))
            .err(),
        Some(PublicationError::Registry)
    );
}

/// Review C21-R1-1: a client counts the hex of every page against a running
/// cap before decoding it, so a server serving large rows is refused once
/// past it, not after the whole log is held.
#[test]
fn pages_past_the_running_byte_cap_are_refused_before_decoding() {
    let p = example_chained();
    let e = LogExpect::default();
    let head = chained_log_body(&p, 0).expect("head");
    let page = log_rows_page(&p, 0, 0).expect("page");
    let size = page["rows"].as_str().expect("hex").len() as u64;
    let mut ok = PagedLog::start(&head, &e)
        .expect("start")
        .with_byte_cap(size);
    assert_eq!(ok.take_rows(&page), Ok(()));
    let mut small = PagedLog::start(&head, &e)
        .expect("start")
        .with_byte_cap(size - 1);
    assert_eq!(small.take_rows(&page).err(), Some(LogError::TooLarge));
    assert!(MAX_PAGED_LOG_BYTES >= 2 * MAX_LOG_PAGE_BYTES as u64);
}

/// Review C21-R2-4: the posts-page bound never refuses a server that fills
/// its pages as `log_posts_page` does (simulated over batches of random
/// sizes: empty, small, one past a page), and refuses one that serves a post
/// a page.
#[test]
fn posts_pages_cut_short_are_refused_and_full_ones_never() {
    let mut x = 0x2c21u64;
    let mut next = |n: u64| {
        x = x
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (x >> 33) % n
    };
    for _ in 0..200 {
        let batches: Vec<u64> = (0..(1 + next(20_000)))
            .map(|_| match next(10) {
                0..=3 => 0,
                4..=8 => next(300),
                _ => LOG_PAGE_ROWS + next(2_000),
            })
            .collect();
        // The server's rule, page by page; the bound holds after every page.
        let (mut at, mut pages, mut rows) = (0usize, 0u64, 0u64);
        while at < batches.len() {
            let (mut n, mut r) = (0u64, 0u64);
            while at < batches.len() {
                let b = batches[at];
                if n > 0 && (r + b > LOG_PAGE_ROWS || n >= LOG_PAGE_ROWS) {
                    break;
                }
                (n, r, at) = (n + 1, r + b, at + 1);
            }
            pages += 1;
            rows += r;
            assert!(
                posts_pages_within(pages, rows, at as u64),
                "{pages} {rows} {at}"
            );
        }
    }
    // A post a page over small batches: refused long before the end.
    let (mut pages, mut rows) = (0u64, 0u64);
    let mut refused = None;
    for k in 1..=10_000u64 {
        pages += 1;
        rows += 1;
        if !posts_pages_within(pages, rows, k) {
            refused = Some(k);
            break;
        }
    }
    assert_eq!(refused, Some(2));
}
