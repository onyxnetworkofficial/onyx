//! Served logs and history pages (ROADMAP C20) against a lying server:
//! outcomes the logs do not give (TR-S-13), a rewrite chained to a known
//! commitment (TR-S-14), a cut or padded paged history (TR-S-2).

use super::*;
use crate::example::{self, AS_OF, AS_OF_EARLY, FORECASTER, METRIC};
use crate::publication::Publication;
use crate::wire::{check_record_body, record_body, Expect};
use crate::{ExtendError, MetricRule, Resolution, TrackBook};
use onyx_core::{Confidence, Subject, Value};

fn publication() -> Publication {
    let (p, reg) = (example::params(), example::registry());
    let mut out = Publication::new();
    for as_of in [AS_OF_EARLY, AS_OF] {
        let (s, r) = example::logs(as_of);
        let book = compute(&p, &reg, &s, &r, as_of).expect("compute");
        out.push(&p, &s, &r, &book, None).expect("push");
    }
    out
}

fn with_registry(reg: &dyn Registry) -> LogExpect<'_> {
    LogExpect {
        registry: Some(reg),
        ..Default::default()
    }
}

#[test]
fn served_logs_rerun_to_the_commitment() {
    let p = publication();
    let reg = example::registry();
    let [(c1, _), (c2, _)] = example::chain().expect("chain");
    // Without `since`: the latest link only, rerun.
    let b = log_body(&p, 1).expect("body");
    let got = check_log_body(&b, &with_registry(&reg)).expect("checks");
    assert_eq!(got.commitment, c2);
    assert!(got.recomputed && got.example && !got.has_intake && !got.intake_checked);
    assert_eq!(
        (got.links, got.statements.len()),
        (1, example::logs(AS_OF).0.len())
    );
    // With `since` the first commitment: both links, the second extending it.
    let b = log_body(&p, 0).expect("body");
    let e = LogExpect {
        known: Some(c1.hash()),
        latest: Some(c2.hash()),
        min_as_of: AS_OF,
        ..with_registry(&reg)
    };
    assert_eq!(check_log_body(&b, &e).expect("checks").links, 2);
    // Without the registry only hashes and extensions are checked.
    let got = check_log_body(&b, &LogExpect::default()).expect("checks");
    assert!(!got.recomputed);
    // A registry with another root: said so, not a silent pass.
    let mut other = example::registry();
    other.add_aggregate("example.other").expect("aggregate");
    assert_eq!(
        check_log_body(&b, &with_registry(&other)).err(),
        Some(LogError::RegistryDiffers)
    );
}

/// TR-S-13: a commitment whose rows were computed from other answers than the
/// logs it commits to (a miss turned into an agreement) passes the node's
/// row check and every proof, and only a rerun on the served logs refuses it.
#[test]
fn a_record_the_logs_do_not_give_is_refused_by_the_rerun() {
    let (params, reg) = (example::params(), example::registry());
    let (s, r) = example::logs(AS_OF);
    // The dishonest computation: the forecaster's day-3 miss "resolved" to
    // its stated value.
    let missed = s
        .iter()
        .find(|x| x.source.0 == FORECASTER && x.fact.as_of == example::T0 + 3 * 86_400)
        .expect("day 3");
    let mut forged = r.clone();
    for x in forged.iter_mut().filter(|x| x.fact == missed.fact) {
        x.value = missed.value.clone();
    }
    crate::sort_resolutions(&mut forged);
    let bad = compute(&params, &reg, &s, &forged, AS_OF).expect("compute");
    let mut c = bad.commitment(Hash32::ZERO);
    c.resolutions_hash = crate::resolutions_hash(&r);
    let p = Publication {
        version: crate::publication::PUBLICATION_VERSION,
        format: crate::chained::LOG_FORMAT_WHOLE,
        registry: None,
        steps: vec![Step {
            commitment: c.clone(),
            params: params.clone(),
            statements: s.len() as u64,
            resolutions: r.len() as u64,
        }],
        statements: s,
        resolutions: r,
        entries: bad.entries().to_vec(),
        records: bad.records().to_vec(),
        metrics: bad.metrics().to_vec(),
        intake: None,
    };
    // The node (no registry) takes it, and the record body checks.
    let book: TrackBook = p.check().expect("rows match the commitment");
    let body = record_body(
        &Served {
            book: &book,
            commitment: &c,
            params: &params,
            chain: None,
        },
        &SourceId(FORECASTER.into()),
        &MetricId(METRIC.into()),
    )
    .expect("record");
    let e = Expect {
        source: FORECASTER,
        metric: METRIC,
        min_as_of: 0,
        known: None,
        latest: None,
    };
    assert!(check_record_body(&body, &e).is_ok());
    // The rerun does not.
    let b = log_body(&p, 0).expect("body");
    assert!(check_log_body(&b, &LogExpect::default()).is_ok());
    assert_eq!(
        check_log_body(&b, &with_registry(&reg)).err(),
        Some(LogError::Recompute)
    );
}

/// TR-S-14: a server drops a committed statement, recomputes and chains the
/// new commitment to the one the client knew. The record body passes the hash
/// chain; the logs refuse it whichever way they are laid out.
#[test]
fn a_rewrite_chained_to_the_known_commitment_is_refused() {
    let (params, reg) = (example::params(), example::registry());
    let honest = publication();
    let c1 = honest.steps[0].commitment.clone();
    let (s1, _) = example::logs(AS_OF_EARLY);
    let (mut s2, r2) = example::logs(AS_OF);
    let dropped = s1[0].clone();
    s2.retain(|x| *x != dropped);
    let fork_book = compute(&params, &reg, &s2, &r2, AS_OF).expect("compute");
    let fork = fork_book.commitment(c1.hash());

    // Laid out with the first step honest: the fork's log must cover the
    // dropped statement too, and does not hash to the fork.
    let mut p = honest.clone();
    p.steps[1].commitment = fork.clone();
    p.entries = fork_book.entries().to_vec();
    p.records = fork_book.records().to_vec();
    p.metrics = fork_book.metrics().to_vec();
    let e = LogExpect {
        known: Some(c1.hash()),
        ..Default::default()
    };
    assert_eq!(
        check_log_body(&log_body(&p, 0).expect("body"), &e).err(),
        Some(LogError::Steps(PublicationError::Log(1)))
    );
    // Laid out as the fork's own log: the known commitment's prefix is not
    // its log any more.
    let mut q = p.clone();
    q.statements.retain(|x| *x != dropped);
    q.steps[0].statements -= 1;
    q.steps[1].statements = q.statements.len() as u64;
    assert_eq!(
        check_log_body(&log_body(&q, 0).expect("body"), &e).err(),
        Some(LogError::Steps(PublicationError::Log(0)))
    );
    // A server that hides the known commitment from the links is refused.
    assert_eq!(
        check_log_body(&log_body(&q, 1).expect("body"), &e).err(),
        Some(LogError::NotDescendant)
    );
    // And the honest chain extends.
    assert!(check_log_body(&log_body(&honest, 0).expect("body"), &e).is_ok());
}

/// Review C20-R1-7: what only `check_extends` catches, through a served body.
/// A server appends a stronger (level-1) answer on a fact the known
/// commitment already resolved, recomputes and chains the result to it: every
/// log hashes to its commitment, and the extension check refuses it.
#[test]
fn a_stronger_answer_rewriting_a_committed_outcome_is_refused() {
    let (params, reg) = (example::params(), example::registry());
    let honest = publication();
    let c1 = honest.steps[0].commitment.clone();
    let (s2, mut r2) = example::logs(AS_OF);
    let (_, r1) = example::logs(AS_OF_EARLY);
    let judged = r1[0].fact.clone();
    let stronger = Resolution {
        fact: judged,
        value: Value::UInt(7),
        level: onyx_core::ProofLevel::Recomputed,
        resolver: SourceId(example::SIGNER.into()),
        evidence: Hash32([5; 32]),
        public_from: AS_OF,
    };
    r2.push(stronger.clone());
    crate::sort_resolutions(&mut r2);
    let book = compute(&params, &reg, &s2, &r2, AS_OF).expect("compute");
    let mut p = honest.clone();
    p.resolutions.push(stronger);
    p.steps[1].commitment = book.commitment(c1.hash());
    p.steps[1].resolutions = p.resolutions.len() as u64;
    p.entries = book.entries().to_vec();
    p.records = book.records().to_vec();
    p.metrics = book.metrics().to_vec();
    let e = LogExpect {
        known: Some(c1.hash()),
        ..Default::default()
    };
    assert_eq!(
        check_log_body(&log_body(&p, 0).expect("body"), &e).err(),
        Some(LogError::Steps(PublicationError::Extend(
            1,
            ExtendError::ResolutionRewrites
        )))
    );
}

/// Review C20-R1-3: a body past the row cap or the check budget (links times
/// rows) is refused before any hashing.
#[test]
fn a_body_past_the_client_limits_is_refused_before_hashing() {
    let p = publication();
    // 2,000 rows (under the row cap) over 4,097 links: past the check budget.
    let mut mid = p.clone();
    let filler = mid.statements[0].clone();
    mid.statements = vec![filler; 2_000];
    let mut b = log_body(&mid, 0).expect("body");
    let link = b["links"][1].clone();
    b["links"] = json!(vec![link; MAX_CHAIN + 1]);
    assert_eq!(
        check_log_body(&b, &LogExpect::default()).err(),
        Some(LogError::TooLarge)
    );
    // Review C20-R2-1: the budget is 64 links over the full 16,384 rows
    // (measured 4.6 s native); 65 links over them are refused unhashed.
    let mut full = p.clone();
    let filler = full.statements[0].clone();
    full.statements = vec![filler; MAX_LOG_ROWS as usize - full.resolutions.len()];
    let mut b = log_body(&full, 0).expect("body");
    let link = b["links"][1].clone();
    for (links, too_large) in [(64usize, false), (65, true)] {
        b["links"] = json!(vec![link.clone(); links]);
        let got = check_log_body(&b, &LogExpect::default()).err();
        assert_eq!(
            got == Some(LogError::TooLarge),
            too_large,
            "{links}: {got:?}"
        );
    }
    let mut big = p.clone();
    let filler = big.statements[0].clone();
    big.statements = vec![filler; MAX_LOG_ROWS as usize + 1];
    assert_eq!(
        check_log_body(&log_body(&big, 1).expect("body"), &LogExpect::default()).err(),
        Some(LogError::TooLarge)
    );
}

#[test]
fn a_malformed_or_mislabelled_log_is_refused() {
    let p = publication();
    let good = log_body(&p, 0).expect("body");
    let d = LogExpect::default();
    let mut extra = good.clone();
    extra["note"] = json!("x");
    assert_eq!(
        check_log_body(&extra, &d).err(),
        Some(LogError::Malformed("log body"))
    );
    let mut flag = good.clone();
    flag["example"] = json!(false);
    assert_eq!(check_log_body(&flag, &d).err(), Some(LogError::ExampleFlag));
    let mut asof = good.clone();
    asof["asOf"] = json!(AS_OF + 1);
    assert_eq!(check_log_body(&asof, &d).err(), Some(LogError::Commitment));
    let mut upper = good.clone();
    upper["statements"] = json!(good["statements"]
        .as_str()
        .expect("hex")
        .to_uppercase()
        .replacen("0X", "0x", 1));
    assert!(matches!(
        check_log_body(&upper, &d).err(),
        Some(LogError::Malformed(_))
    ));
    let floor = LogExpect {
        min_as_of: AS_OF + 1,
        ..d
    };
    assert!(matches!(
        check_log_body(&good, &floor).err(),
        Some(LogError::TooOld { .. })
    ));
    let pin = LogExpect {
        latest: Some(p.steps[0].commitment.hash()),
        ..d
    };
    assert_eq!(check_log_body(&good, &pin).err(), Some(LogError::NotLatest));
    let mut none = good.clone();
    none["links"] = json!([]);
    assert_eq!(
        check_log_body(&none, &d).err(),
        Some(LogError::Malformed("links"))
    );
    // A real publication called the example is refused too.
    let mut real = p.clone();
    for s in &mut real.steps {
        s.params.rules[0].metric = MetricId("test.real".into());
    }
    let b = log_body(&real, 0).expect("body");
    assert_eq!(b["example"], json!(false));
}

/// A track with one record of 1,300 entries (past `MAX_HISTORY`): unresolved
/// daily statements on six subjects.
fn big() -> (TrackParams, TrackCommitment, TrackBook) {
    let params = example::params();
    let reg = example::registry();
    let mut s = Vec::new();
    for d in 0..217u64 {
        for e in example::ENTITIES {
            let as_of = example::T0 + d * 86_400;
            s.push(Statement {
                source: SourceId(FORECASTER.into()),
                fact: crate::Fact {
                    metric: MetricId(METRIC.into()),
                    subject: Subject::Entity(e.into()),
                    as_of,
                },
                value: Value::UInt(u128::from(d)),
                confidence: Confidence(700_000),
                committed_at: as_of - 60,
            });
        }
    }
    crate::sort_statements(&mut s);
    let as_of = example::T0 + 300 * 86_400;
    let book = compute(&params, &reg, &s, &[], as_of).expect("compute");
    let c = book.commitment(Hash32::ZERO);
    let _ = MetricRule::after;
    (params, c, book)
}

fn served<'a>(p: &'a TrackParams, c: &'a TrackCommitment, b: &'a TrackBook) -> Served<'a> {
    Served {
        book: b,
        commitment: c,
        params: p,
        chain: None,
    }
}

/// C20 item 3: a history past 1,024 entries is served in pages; the client
/// recounts the record over all of them.
#[test]
fn a_large_history_is_checked_over_its_pages() {
    let (p, c, b) = big();
    let (src, met) = (SourceId(FORECASTER.into()), MetricId(METRIC.into()));
    let s = served(&p, &c, &b);
    let body = record_body(&s, &src, &met).expect("record");
    assert_eq!(body["history"], Json::Null);
    let e = Expect {
        source: FORECASTER,
        metric: METRIC,
        min_as_of: 0,
        known: None,
        latest: None,
    };
    let mut checked = check_record_body(&body, &e).expect("record");
    assert!(!checked.history_checked);
    assert_eq!(checked.record.entries_count, 1_302);
    let pages: Vec<HistoryPage> = [0u64, MAX_HISTORY]
        .iter()
        .map(|&from| {
            let pb = history_page_body(&s, &src, &met, from).expect("page");
            check_history_page(&pb, &checked, from).expect("page checks")
        })
        .collect();
    assert_eq!(pages[0].entries.len() as u64, MAX_HISTORY);
    assert!(history_page_body(&s, &src, &met, 1_302).is_none());
    let mut cut = checked.clone();
    assert_eq!(
        complete_history(&mut cut, &pages[..1]),
        Err(WireError::Malformed("pages"))
    );
    let mut swapped = vec![pages[1].clone(), pages[0].clone()];
    assert_eq!(
        complete_history(&mut cut, &swapped),
        Err(WireError::Malformed("pages"))
    );
    // A page with one entry replaced by its neighbour's row.
    swapped = pages.clone();
    swapped[1].entries[5].0 = swapped[1].entries[4].0.clone();
    assert!(complete_history(&mut cut, &swapped).is_err());
    complete_history(&mut checked, &pages).expect("recount");
    assert!(checked.history_checked);
    assert_eq!(checked.entries.len(), 1_302);
}

#[test]
fn a_page_of_another_commitment_or_place_is_refused() {
    let (p, c, b) = big();
    let (src, met) = (SourceId(FORECASTER.into()), MetricId(METRIC.into()));
    let s = served(&p, &c, &b);
    let e = Expect {
        source: FORECASTER,
        metric: METRIC,
        min_as_of: 0,
        known: None,
        latest: None,
    };
    let checked = check_record_body(&record_body(&s, &src, &met).expect("r"), &e).expect("r");
    let page = history_page_body(&s, &src, &met, 0).expect("page");
    assert!(matches!(
        check_history_page(&page, &checked, 1),
        Err(WireError::Malformed("from"))
    ));
    let mut other = page.clone();
    other["commitmentHash"] = json!(Hash32([7; 32]).to_string());
    assert_eq!(
        check_history_page(&other, &checked, 0).err(),
        Some(WireError::Commitment)
    );
    let mut shifted = page.clone();
    shifted["from"] = json!(1);
    assert!(check_history_page(&shifted, &checked, 1).is_err());
    let mut extra = page;
    extra["x"] = json!(1);
    assert!(check_history_page(&extra, &checked, 0).is_err());
}
