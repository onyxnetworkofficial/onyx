//! The recorded EDGAR rounds settle at level 2, the query API serves them, a
//! client holding its own copy of the registered template verifies them, and
//! their on-chain fact keys are those of a period-dated metric.

use onyx_api::client::{verify_observation, ObservationExpect};
use onyx_api::observations::{ObservationStore, TemplatePins};
use onyx_coordinator::observe::onchain_observation_check;
use onyx_coordinator::observe::SimulatedRegistry;
use onyx_core::onchain::{metric_key, MetricKind};
use onyx_core::{Answer, ProofLevel, Subject};
use onyx_source_edgar::round::{results_store, run, store, Page, RoundRun};
use onyx_source_edgar::{recorded, subject, template, Recording};

fn pages(rec: &Recording) -> Vec<Page> {
    rec.fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect()
}

fn rounds() -> Vec<(Recording, RoundRun)> {
    let mut chain = SimulatedRegistry::new();
    let mut s = store();
    recorded()
        .into_iter()
        .map(|rec| {
            let r = run(
                &rec.company,
                &rec.period_end,
                rec.window_start,
                &pages(&rec),
                &mut s,
                &mut chain,
            )
            .expect("round settles");
            (rec, r)
        })
        .collect()
}

#[test]
fn every_company_round_settles_at_level_2_with_the_filed_value() {
    for ((rec, r), want) in rounds().iter().zip([762_099i128, 15_679]) {
        assert_eq!(
            r.value().expect("value").value,
            onyx_core::Value::Int(want),
            "{}",
            rec.company.key
        );
        assert_eq!(
            r.value().expect("value").method.proof_level(),
            ProofLevel::ReObserved
        );
    }
}

#[test]
fn a_second_round_for_the_same_period_is_refused() {
    let mut chain = SimulatedRegistry::new();
    let mut s = store();
    let rec = &recorded()[0];
    run(
        &rec.company,
        &rec.period_end,
        rec.window_start,
        &pages(rec),
        &mut s,
        &mut chain,
    )
    .expect("first");
    // Same period, a later window: one fact per period (L2SRC-R2-F2).
    let later: Vec<Page> = pages(rec)
        .into_iter()
        .map(|p| Page {
            fetched_unix: p.fetched_unix + 600,
            body: p.body,
        })
        .collect();
    assert!(run(
        &rec.company,
        &rec.period_end,
        rec.window_start + 600,
        &later,
        &mut s,
        &mut chain
    )
    .is_err());
}

#[test]
fn the_api_serves_each_round_and_the_client_verifies_it() {
    for (rec, r) in rounds() {
        let c = rec.company;
        let t = template(&c);
        let pins = TemplatePins::from([(t.metric.0.clone(), t.id())]);
        let now = rec.window_start + 86_400;
        let text = results_store(&c, &r).expect("store").to_string();
        let feed = ObservationStore::from_json(&text, now, &pins).expect("loads and re-checks");
        let sub = subject(&c, &rec.period_end);
        let served = feed.record(&sub, None, None).expect("record");
        assert_eq!(served.checked.result, r.settlement.result_hash());
        let body = feed.body(served);
        let subj = Subject::Aggregate(sub.clone());
        let v = verify_observation(
            &body,
            &ObservationExpect {
                template: &t,
                subject: &subj,
                window_end: None,
                min_window_end: rec.window_start,
                now,
            },
        )
        .expect("verifies");
        let Answer::Number(n) = v.answer else {
            panic!("a number")
        };
        assert_eq!(onyx_core::Value::Int(n), r.value().expect("value").value);
        // The price clock is the template's: period end + 110 days + window.
        let day =
            u64::try_from(onyx_core::day_number(&rec.period_end).expect("date")).expect("pos");
        assert_eq!(
            t.decay_start(&subj, 0),
            Some((day + u64::from(onyx_source_edgar::MIN_DAYS)) * 86_400 + 300)
        );
        // A client whose template names another unit refuses the answer.
        let mut other = t.clone();
        other.selector = "units/USD/end={date}".into();
        assert!(verify_observation(
            &body,
            &ObservationExpect {
                template: &other,
                subject: &subj,
                window_end: None,
                min_window_end: rec.window_start,
                now,
            },
        )
        .is_err());
        // A store pinned to another template does not load.
        let wrong = TemplatePins::from([(t.metric.0.clone(), other.id())]);
        assert!(ObservationStore::from_json(&text, now, &wrong).is_err());
    }
}

#[test]
fn on_chain_the_metric_is_a_dated_feed_and_the_fact_key_checks() {
    for (rec, r) in rounds() {
        let t = template(&rec.company);
        assert_eq!(MetricKind::of(&t), MetricKind::DatedFeed);
        assert_ne!(metric_key(&t.metric), onyx_core::Hash32::ZERO);
        let tr = onyx_source_edgar::round::transcript(&r).expect("transcript");
        let ok = onchain_observation_check(
            &t,
            MetricKind::DatedFeed,
            &t.id(),
            &tr.subject,
            tr.window_start,
            &tr.cfg,
            &tr.pool,
            &tr.draw,
            &tr.observers,
            &tr.commits,
            &tr.reveals,
        )
        .expect("a consumer's own check passes");
        assert_eq!(ok.result, r.settlement.result_hash());
        // Registered as a non-dated feed, the consumer refuses it.
        assert!(onchain_observation_check(
            &t,
            MetricKind::Feed,
            &t.id(),
            &tr.subject,
            tr.window_start,
            &tr.cfg,
            &tr.pool,
            &tr.draw,
            &tr.observers,
            &tr.commits,
            &tr.reveals,
        )
        .is_err());
    }
}

/// SEC-R1-F1: a round that settles no value (EDGAR refused the observers, a
/// late filer had not filed, a disagreeing amended row) leaves the period
/// open: a later round settles the real filing, and that value then closes
/// the period for good.
#[test]
fn an_unavailable_round_leaves_the_period_open_for_the_value() {
    let mut chain = SimulatedRegistry::new();
    let mut s = store();
    let rec = &recorded()[0];
    let blocked: Vec<Page> = rec
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(b"<html>403 Forbidden: undeclared automated tool</html>".to_vec()),
        })
        .collect();
    let first = run(
        &rec.company,
        &rec.period_end,
        rec.window_start,
        &blocked,
        &mut s,
        &mut chain,
    );
    let e = first.expect("runs").value().expect_err("no value");
    assert!(e.contains("SourceUnavailable"), "{e}");
    let pages = |shift: u64| -> Vec<Page> {
        rec.fetches
            .iter()
            .map(|f| Page {
                fetched_unix: f.fetched_unix + shift,
                body: Some(f.body.clone()),
            })
            .collect()
    };
    // A day later EDGAR answers with the real filing: the value settles.
    let second = run(
        &rec.company,
        &rec.period_end,
        rec.window_start + 86_400,
        &pages(86_400),
        &mut s,
        &mut chain,
    )
    .expect("the period was still open");
    assert_eq!(
        second.value().expect("value").value,
        onyx_core::Value::Int(762_099)
    );
    // Only a value outcome is ever recorded on chain.
    assert!(s.onchain_record(&second.settlement.result_hash()).is_some());
    // Once a value settled, no later round can settle the period again.
    let third = run(
        &rec.company,
        &rec.period_end,
        rec.window_start + 2 * 86_400,
        &pages(2 * 86_400),
        &mut s,
        &mut chain,
    );
    let e = third.expect_err("closed by the value");
    assert!(e.error.to_lowercase().contains("settled"), "{e}");
}
