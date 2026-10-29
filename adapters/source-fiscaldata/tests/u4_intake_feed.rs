//! PRODUCT-PLAN.md U4: one approved submission through the real level-2 pipeline. The approved
//! Treasury example (fixtures/intake/vectors.json) becomes the adapter's pinned observer round on
//! the recorded fetches; anything else is refused before a round runs.

use onyx_api::observations::ObservationStore;
use onyx_api::submit::review::ReviewItem;
use onyx_coordinator::observe::SimulatedRegistry;
use onyx_intake::Submission;
use onyx_source_fiscaldata::intake_feed::{approved_record_date, feed_approved, FeedError};
use onyx_source_fiscaldata::round::{results_store, store, Page};
use onyx_source_fiscaldata::{record_url, recorded, subject, template, RECORDED_WINDOW_START};

fn example() -> serde_json::Value {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/intake/vectors.json"))
            .expect("vectors");
    v["example"].clone()
}

fn item(status: &str, edit: impl FnOnce(&mut serde_json::Value)) -> ReviewItem {
    let mut s = example();
    edit(&mut s);
    ReviewItem {
        id: format!("sub_{}", "4".repeat(32)),
        hash: format!("0x{}", "ab".repeat(32)),
        status: status.into(),
        revision: 1,
        received_unix: RECORDED_WINDOW_START - 3_600,
        reviewed_unix: Some(RECORDED_WINDOW_START - 1_800),
        reason: Some("reviewed".into()),
        submission: serde_json::from_value::<Submission>(s).expect("the example parses"),
    }
}

fn pages() -> Vec<Page> {
    recorded()
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect()
}

#[test]
fn an_approved_treasury_submission_runs_the_pinned_observer_round() {
    let it = item("approved", |_| {});
    assert_eq!(recorded().record_date, "2026-10-08");
    let mut s = store();
    let mut chain = SimulatedRegistry::new();
    let fed = feed_approved(&it, RECORDED_WINDOW_START, &pages(), &mut s, &mut chain).expect("fed");
    assert_eq!(fed.record_date, "2026-10-08");
    assert_eq!(fed.submission_id, it.id);
    // The round fetched the adapter's pinned URL, never the submitter's spelling of it.
    assert_eq!(fed.run.spec.url, record_url("2026-10-08"));
    // Served as the query API serves it: the store re-checks the round against the template.
    let text = results_store(&fed.run).expect("store").to_string();
    let pins = onyx_api::observations::TemplatePins::from([(
        template().metric.0.clone(),
        template().id(),
    )]);
    let feed = ObservationStore::from_json(&text, RECORDED_WINDOW_START + 86_400, &pins)
        .expect("loads and re-checks");
    let body = feed.body(
        feed.record(&subject("2026-10-08"), None, None)
            .expect("record"),
    );
    assert_eq!(
        body["value"], "40305316210829.72",
        "the checked value is the observers' value"
    );
    // A retry with the same inputs gives the same round and the same result.
    let mut s2 = store();
    let mut chain2 = SimulatedRegistry::new();
    let again = feed_approved(&it, RECORDED_WINDOW_START, &pages(), &mut s2, &mut chain2)
        .expect("fed again");
    assert_eq!(again.round_id, fed.round_id);
    assert_eq!(again.result, fed.result);
}

#[test]
fn only_an_approved_submission_is_fed() {
    for status in ["pending_review", "rejected", "held_rights_review"] {
        assert!(
            matches!(
                approved_record_date(&item(status, |_| {})),
                Err(FeedError::NotApproved(_))
            ),
            "{status}"
        );
    }
}

#[test]
fn anything_but_the_pinned_feed_is_refused() {
    let base = "https://api.fiscaldata.treasury.gov/services/api/fiscal_service/v2/accounting/od/debt_to_penny";
    let refused = [
        format!("{base}?filter=record_date:eq:2026-10-08&page[size]=100"),
        format!("{base}?filter=record_date:gte:2026-10-08"),
        format!("{base}?filter=record_date:eq:2026-10-08&filter=record_date:eq:2026-10-09"),
        format!("{base}?fields=debt_held_public_amt&filter=record_date:eq:2026-10-08"),
        format!("{base}/other?filter=record_date:eq:2026-10-08"),
        format!("{base}?filter=record_date:eq:26-10-08"),
        "https://api.fiscaldata.treasury.gov/services/api/fiscal_service/v2/accounting/od/avg_interest_rates?filter=record_date:eq:2026-10-08".into(),
    ];
    for url in refused {
        let it = item("approved", |s| s["endpointUrl"] = url.clone().into());
        assert!(
            matches!(approved_record_date(&it), Err(FeedError::NotThePinnedFeed)),
            "{url}"
        );
    }
    let pinned = item("approved", |s| {
        s["endpointUrl"] = record_url("2026-10-08").into()
    });
    assert_eq!(
        approved_record_date(&pinned).expect("the pinned URL itself"),
        "2026-10-08"
    );
}

#[test]
fn another_field_or_unclear_rights_are_refused() {
    let it = item("approved", |s| {
        s["jsonPointer"] = "/data/0/debt_held_public_amt".into()
    });
    assert!(matches!(
        approved_record_date(&it),
        Err(FeedError::OtherField)
    ));
    for rights in ["own_data", "not_sure"] {
        let it = item("approved", |s| {
            s["answers"]["rights"] = rights.into();
            if let Some(a) = s["answers"].as_object_mut() {
                a.remove("licenseId");
            }
        });
        assert!(
            matches!(approved_record_date(&it), Err(FeedError::Rights)),
            "{rights}"
        );
    }
}

#[test]
fn the_feed_ledger_feeds_once_and_bounds_failures() {
    use onyx_source_fiscaldata::intake_feed::{FeedLedger, FeedStep, MAX_FEED_ATTEMPTS};
    let it = item("approved", |_| {});
    let mut ledger = FeedLedger::default();
    assert_eq!(ledger.step(&it.id, &it.hash), Ok(FeedStep::Run));
    // Failures keep their reason; the third one is the last.
    for n in 1..=MAX_FEED_ATTEMPTS {
        assert_eq!(ledger.failed(&it.id, "the chain could not be read"), n);
    }
    assert_eq!(
        ledger.step(&it.id, &it.hash),
        Ok(FeedStep::GiveUp("the chain could not be read".into()))
    );
    // A fed submission returns its evidence and is never run again; other content is refused.
    let mut fresh = FeedLedger::default();
    let mut s = store();
    let mut chain = SimulatedRegistry::new();
    let fed = feed_approved(&it, RECORDED_WINDOW_START, &pages(), &mut s, &mut chain).expect("fed");
    fresh.fed(&fed.evidence("simulated"));
    assert!(matches!(
        fresh.step(&it.id, &it.hash),
        Ok(FeedStep::AlreadyFed(_))
    ));
    assert!(fresh.step(&it.id, "0xother").is_err());
    // The ledger file round-trips and refuses anything else.
    let back = FeedLedger::from_json(&fresh.to_json()).expect("round-trip");
    assert_eq!(back, fresh);
    assert!(FeedLedger::from_json(&serde_json::json!({"x": {"state": "paid"}})).is_err());
    assert!(FeedLedger::from_json(&serde_json::json!([1])).is_err());
    // Fed evidence is served publicly: a malformed field or id is refused, not served.
    let mut bad = fresh.to_json();
    bad[&it.id]["roundId"] = "0xnot-a-hash".into();
    assert!(FeedLedger::from_json(&bad).is_err());
    let mut bad = fresh.to_json();
    bad[&it.id]["recordDate"] = "<script>".into();
    assert!(FeedLedger::from_json(&bad).is_err());
    let mut bad = fresh.to_json();
    bad[&it.id]["registry"] = "eip155:0x14a34".into();
    assert!(FeedLedger::from_json(&bad).is_err());
    let moved = serde_json::json!({ "not-an-id": fresh.to_json()[&it.id].clone() });
    assert!(FeedLedger::from_json(&moved).is_err());
    // Public evidence: only for this approved content.
    let check = fresh
        .public_check(&it.id, "approved", &it.hash)
        .expect("fed and approved");
    assert_eq!(check["roundId"], fed.round_id.to_string());
    assert_eq!(check["result"], fed.result.to_string());
    assert_eq!(check["recordDate"], "2026-10-08");
    assert_eq!(
        check["value"], "40305316210829.72",
        "PP11: the observers' value"
    );
    assert_eq!(check["level"], 2);
    assert_eq!(check["registry"], "simulated");
    assert!(fresh.public_check(&it.id, "rejected", &it.hash).is_none());
    assert!(fresh.public_check(&it.id, "approved", "0xother").is_none());
    assert!(
        ledger.public_check(&it.id, "approved", &it.hash).is_none(),
        "failed: nothing"
    );
}

/// PP9: a live round's store (here the recorded round's, tagged with a chain as the live
/// coordinator writes it) feeds the approved submission's evidence, with the chain the store
/// names; an untagged store, another date's submission, or an unapproved one does not.
#[test]
fn a_live_rounds_store_feeds_the_evidence_of_its_date_only() {
    use onyx_source_fiscaldata::intake_feed::fed_from_store;
    let it = item("approved", |_| {});
    let mut s = store();
    let mut chain = SimulatedRegistry::new();
    let fed = feed_approved(&it, RECORDED_WINDOW_START, &pages(), &mut s, &mut chain).expect("fed");
    let mut v = results_store(&fed.run).expect("store");
    let untagged = v.to_string();
    v["onchain"] =
        serde_json::json!({"chainId": 84532, "registry": format!("0x{}", "11".repeat(20))});
    let text = v.to_string();
    let now = RECORDED_WINDOW_START + 86_400;
    let e = fed_from_store(&it, &text, now).expect("evidence");
    assert_eq!(e.round_id, fed.round_id.to_string());
    assert_eq!(e.result, fed.result.to_string());
    assert_eq!(e.record_date, "2026-10-08");
    assert_eq!(
        e.value, fed.value,
        "the live path reads the same value from the store"
    );
    assert_eq!(e.value, "40305316210829.72");
    assert_eq!(
        e.registry, "eip155:84532",
        "the store names the chain, not the operator"
    );
    assert!(
        matches!(
            fed_from_store(&it, &untagged, now),
            Err(FeedError::Round(_))
        ),
        "an untagged store is no live round"
    );
    let other = item("approved", |s| {
        s["endpointUrl"] = record_url("2026-10-09").into()
    });
    assert!(matches!(
        fed_from_store(&other, &text, now),
        Err(FeedError::Round(_))
    ));
    let pending = item("pending_review", |_| {});
    assert!(matches!(
        fed_from_store(&pending, &text, now),
        Err(FeedError::NotApproved(_))
    ));
    assert!(fed_from_store(&it, "{}", now).is_err(), "not a store");
}
