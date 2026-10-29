//! The served form of a record (ROADMAP C6) against a lying server: truncated
//! or padded histories, forged counts, another record, a stale or forked
//! commitment, a real record dressed as the example (THREATS TR-S-*).

use super::*;
use crate::example::{self, AS_OF, AS_OF_EARLY, FORECASTER, GUESSER, METRIC};

fn chain() -> [(TrackCommitment, TrackBook); 2] {
    example::chain().expect("example chain")
}

fn body_of(i: usize, source: &str, since: Option<usize>) -> Json {
    let c = chain();
    let p = example::params();
    let after: Vec<TrackCommitment> = match since {
        Some(k) => c[k + 1..=i].iter().map(|(c, _)| c.clone()).collect(),
        None => Vec::new(),
    };
    let s = Served {
        book: &c[i].1,
        commitment: &c[i].0,
        params: &p,
        chain: since.map(|_| after.as_slice()),
    };
    record_body(&s, &SourceId(source.into()), &MetricId(METRIC.into())).expect("record")
}

fn body(source: &str) -> Json {
    body_of(1, source, None)
}

fn expect(source: &str) -> Expect<'_> {
    Expect {
        source,
        metric: METRIC,
        min_as_of: 0,
        known: None,
        latest: None,
    }
}

fn hash_of(i: usize) -> Hash32 {
    chain()[i].0.hash()
}

/// Re-encode the row of `v` (a `{row, proof}` object) after `f` changes it.
fn rewrite<T: BorshDeserialize + borsh::BorshSerialize>(v: &mut Json, f: impl FnOnce(&mut T)) {
    let mut t: T = parse_borsh(&v["row"], "row").expect("row");
    f(&mut t);
    v["row"] = Json::String(borsh_hex(&t));
}

#[test]
fn an_honest_body_checks_for_every_example_record() {
    for s in [FORECASTER, GUESSER] {
        let c = check_record_body(&body(s), &expect(s)).expect("honest body");
        assert!(c.history_checked && c.example);
        assert_eq!(c.entries.len() as u64, c.record.entries_count);
        assert_eq!(c.commitment.as_of, AS_OF);
    }
    assert!(record_body(
        &Served {
            book: &chain()[1].1,
            commitment: &chain()[1].0,
            params: &example::params(),
            chain: None,
        },
        &SourceId("example.nobody".into()),
        &MetricId(METRIC.into()),
    )
    .is_none());
}

/// TR-S-2: dropping, reordering or repeating history entries, or hiding a
/// neighbour, is refused; a small record cannot be served without its history.
#[test]
fn a_truncated_or_padded_history_is_refused() {
    let e = expect(FORECASTER);
    let mut b = body(FORECASTER);
    b["history"]["entries"]
        .as_array_mut()
        .expect("entries")
        .pop();
    assert_eq!(
        check_record_body(&b, &e).err(),
        Some(WireError::Proof(VerifyError::Incomplete))
    );
    let mut b = body(FORECASTER);
    let list = b["history"]["entries"].as_array_mut().expect("entries");
    list.swap(0, 1);
    assert!(matches!(
        check_record_body(&b, &e),
        Err(WireError::Proof(VerifyError::Entry(_)))
    ));
    let mut b = body(FORECASTER);
    let first = b["history"]["entries"][0].clone();
    b["history"]["entries"]
        .as_array_mut()
        .expect("entries")
        .push(first);
    assert!(check_record_body(&b, &e).is_err());
    // The forecaster's record is the first: its `before` is null and its
    // `after` the guesser's first entry. Hiding `after` is refused.
    let mut b = body(FORECASTER);
    assert!(b["history"]["before"].is_null());
    b["history"]["after"] = Json::Null;
    assert_eq!(
        check_record_body(&b, &e).err(),
        Some(WireError::Proof(VerifyError::Incomplete))
    );
    let mut b = body(GUESSER);
    b["history"]["before"] = Json::Null;
    assert_eq!(
        check_record_body(&b, &expect(GUESSER)).err(),
        Some(WireError::Proof(VerifyError::Incomplete))
    );
    let mut b = body(FORECASTER);
    b["history"] = Json::Null;
    assert_eq!(
        check_record_body(&b, &e).err(),
        Some(WireError::Malformed("history"))
    );
}

/// TR-S-3: a record with better counts, an entry with a better outcome, or a
/// summary with fewer sources does not match its proof or the recount.
#[test]
fn forged_counts_and_outcomes_are_refused() {
    let e = expect(FORECASTER);
    let mut b = body(FORECASTER);
    rewrite::<Record>(&mut b["record"], |r| {
        r.agreed += 3;
        r.disagreed -= 3;
    });
    assert_eq!(
        check_record_body(&b, &e).err(),
        Some(WireError::Proof(VerifyError::Record))
    );
    let mut b = body(FORECASTER);
    let k = (0..36)
        .find(|k| {
            let en: Entry = parse_borsh(&b["history"]["entries"][*k]["row"], "e").expect("e");
            en.outcome == crate::Outcome::Disagreed
        })
        .expect("a miss");
    rewrite::<Entry>(&mut b["history"]["entries"][k], |en| {
        en.outcome = crate::Outcome::Agreed;
    });
    assert!(matches!(
        check_record_body(&b, &e),
        Err(WireError::Proof(VerifyError::Entry(_)))
    ));
    let mut b = body(FORECASTER);
    rewrite::<MetricSummary>(&mut b["summary"], |m| m.sources = 1);
    assert_eq!(
        check_record_body(&b, &e).err(),
        Some(WireError::Proof(VerifyError::Summary))
    );
}

/// TR-S-4: the guesser's (valid) record served for the forecaster is refused,
/// whether the body names the guesser or claims the forecaster.
#[test]
fn another_record_is_refused() {
    let b = body(GUESSER);
    assert_eq!(
        check_record_body(&b, &expect(FORECASTER)).err(),
        Some(WireError::WrongRecord)
    );
    let mut b = body(GUESSER);
    b["source"] = Json::String(FORECASTER.into());
    assert_eq!(
        check_record_body(&b, &expect(FORECASTER)).err(),
        Some(WireError::WrongRecord)
    );
}

/// TR-R2-5 (partial): an older commitment served as the latest is refused by
/// a client's floor, and by a client that saw a newer one; a client that saw
/// the older one accepts the newer one only with a chain back to it.
#[test]
fn a_stale_commitment_is_refused_by_the_floor_and_the_known_commitment() {
    let stale = body_of(0, FORECASTER, None);
    assert!(
        check_record_body(&stale, &expect(FORECASTER)).is_ok(),
        "no floor, no known: accepted"
    );
    let floor = Expect {
        min_as_of: AS_OF,
        ..expect(FORECASTER)
    };
    assert_eq!(
        check_record_body(&stale, &floor).err(),
        Some(WireError::TooOld {
            as_of: AS_OF_EARLY,
            min_as_of: AS_OF
        })
    );
    // A client that saw the latest asks with since=latest; the server answers
    // with the older commitment and any chain: refused.
    let knows_latest = Expect {
        known: Some(hash_of(1)),
        ..expect(FORECASTER)
    };
    let mut b = body_of(0, FORECASTER, Some(0));
    assert_eq!(
        check_record_body(&b, &knows_latest).err(),
        Some(WireError::NotDescendant)
    );
    b["chain"] = Json::Null;
    assert_eq!(
        check_record_body(&b, &knows_latest).err(),
        Some(WireError::NotDescendant)
    );
    // Same commitment as known: an empty chain.
    assert!(check_record_body(&body_of(1, FORECASTER, Some(1)), &knows_latest).is_ok());
    // A client that saw the early one gets the latest with the link to it.
    let knows_early = Expect {
        known: Some(hash_of(0)),
        ..expect(FORECASTER)
    };
    let b = body_of(1, FORECASTER, Some(0));
    assert_eq!(b["chain"].as_array().map(Vec::len), Some(1));
    assert!(check_record_body(&b, &knows_early).is_ok());
    let mut forked = b.clone();
    let mut link: TrackCommitment = parse_borsh(&forked["chain"][0], "c").expect("c");
    link.prev = Hash32([7; 32]);
    forked["chain"][0] = Json::String(borsh_hex(&link));
    assert_eq!(
        check_record_body(&forked, &knows_early).err(),
        Some(WireError::NotDescendant)
    );
    let mut missing = b;
    missing["chain"] = Json::Null;
    assert_eq!(
        check_record_body(&missing, &knows_early).err(),
        Some(WireError::NotDescendant)
    );
}

/// TR-S-5: the example flag must match the ids, so neither a real record
/// shown as the example nor the example shown as real passes.
#[test]
fn the_example_flag_must_match_the_ids() {
    let mut b = body(FORECASTER);
    b["example"] = Json::Bool(false);
    assert_eq!(
        check_record_body(&b, &expect(FORECASTER)).err(),
        Some(WireError::ExampleFlag)
    );
}

/// TR-S-6: commitment, hash, `asOf` and params must agree; a strict shape.
#[test]
fn a_mismatched_commitment_or_shape_is_refused() {
    let e = expect(FORECASTER);
    let mut b = body(FORECASTER);
    b["commitmentHash"] = Json::String(hash_of(0).to_string());
    assert_eq!(check_record_body(&b, &e).err(), Some(WireError::Commitment));
    let mut b = body(FORECASTER);
    b["asOf"] = json!(AS_OF + 1);
    assert_eq!(check_record_body(&b, &e).err(), Some(WireError::Commitment));
    let mut b = body(FORECASTER);
    let mut p = example::params();
    p.rules[0].tolerance = 1_000_000;
    b["params"] = Json::String(borsh_hex(&p));
    assert_eq!(check_record_body(&b, &e).err(), Some(WireError::Commitment));
    let mut b = body(FORECASTER);
    b["extra"] = json!(1);
    assert_eq!(
        check_record_body(&b, &e).err(),
        Some(WireError::Malformed("body"))
    );
    let mut b = body(FORECASTER);
    let up = b["commitmentHash"]
        .as_str()
        .expect("hash")
        .to_uppercase()
        .replace("0X", "0x");
    b["commitmentHash"] = Json::String(up);
    assert_eq!(
        check_record_body(&b, &e).err(),
        Some(WireError::Malformed("commitmentHash"))
    );
    let mut b = body(FORECASTER);
    b["level"] = json!(3);
    assert_eq!(
        check_record_body(&b, &e).err(),
        Some(WireError::Malformed("level"))
    );
}

/// TR-S-5 follow-up (review TR-S-R1-5): without `known`, `chain` must be null.
#[test]
fn a_chain_without_since_is_refused() {
    let mut b = body(FORECASTER);
    b["chain"] = json!({ "x": "y".repeat(1024) });
    assert_eq!(
        check_record_body(&b, &expect(FORECASTER)).err(),
        Some(WireError::Malformed("chain"))
    );
}

/// TR-S-14 (review TR-S-R1-1): `known` proves descent by hash only. A server
/// that drops committed statements, recomputes and chains the result to the
/// known commitment passes; only `check_extends` on the logs (C20) refuses it.
/// This test pins the limit so the docs cannot claim more.
#[test]
fn a_fork_chained_to_the_known_commitment_passes_the_hash_chain_only() {
    use crate::{check_extends, compute, TrackLog};
    let [(c1, _), _] = chain();
    let (s1, r1) = example::logs(AS_OF_EARLY);
    let (mut s2, r2) = example::logs(AS_OF);
    let drop = s2
        .iter()
        .position(|s| s.source.0 == FORECASTER)
        .expect("one");
    s2.remove(drop);
    let p = example::params();
    let book = compute(&p, &example::registry(), &s2, &r2, AS_OF).expect("book");
    let forged = book.commitment(c1.hash());
    let log = |s, r| TrackLog {
        params: &p,
        statements: s,
        resolutions: r,
    };
    assert_eq!(
        check_extends(&c1, log(&s1, &r1), &forged, log(&s2, &r2)),
        Err(crate::ExtendError::StatementDropped)
    );
    let chain_after = [forged.clone()];
    let served = Served {
        book: &book,
        commitment: &forged,
        params: &p,
        chain: Some(&chain_after),
    };
    let b = record_body(
        &served,
        &SourceId(FORECASTER.into()),
        &MetricId(METRIC.into()),
    )
    .expect("body");
    let knows = Expect {
        known: Some(c1.hash()),
        ..expect(FORECASTER)
    };
    assert!(
        check_record_body(&b, &knows).is_ok(),
        "the hash chain alone cannot see the rewrite"
    );
    // ADR-0036 amendment 3: a client that reads the on-chain log refuses the
    // fork unless it is the log's latest (the book only takes one successor
    // of c1, chain::tests::only_the_latest_onchain_commitment_is_accepted).
    let onchain = Expect {
        latest: Some(hash_of(1)),
        ..knows
    };
    assert_eq!(
        check_record_body(&b, &onchain).err(),
        Some(WireError::NotLatest)
    );
}

/// TR-R2-5 (ROADMAP C7): with the on-chain log's latest commitment, a client
/// refuses an older one even on its first visit (no floor, no `since`).
#[test]
fn a_client_with_the_onchain_latest_refuses_any_other_commitment() {
    let first_visit = Expect {
        latest: Some(hash_of(1)),
        ..expect(FORECASTER)
    };
    assert!(check_record_body(&body_of(1, FORECASTER, None), &first_visit).is_ok());
    assert_eq!(
        check_record_body(&body_of(0, FORECASTER, None), &first_visit).err(),
        Some(WireError::NotLatest)
    );
}

/// One source's book with `n` statements on `n` distinct facts of the example
/// metric (none resolved), and its first commitment.
fn book_of(n: u64) -> (TrackBook, TrackCommitment) {
    use crate::{compute, sort_statements, Fact, Statement};
    use onyx_core::{Confidence, Subject, Value};
    const DAY: u64 = 86_400;
    let mut s: Vec<Statement> = (0..n)
        .map(|d| Statement {
            source: SourceId(FORECASTER.into()),
            fact: Fact {
                metric: MetricId(METRIC.into()),
                subject: Subject::Entity(example::ENTITIES[(d % 6) as usize].into()),
                as_of: example::T0 + d * DAY,
            },
            value: Value::UInt(1),
            confidence: Confidence(800_000),
            committed_at: example::T0 + d * DAY - 60,
        })
        .collect();
    sort_statements(&mut s);
    let b = compute(
        &example::params(),
        &example::registry(),
        &s,
        &[],
        example::T0 + n * DAY,
    )
    .expect("book");
    let c = b.commitment(Hash32::ZERO);
    (b, c)
}

/// Review TR-S-R2-1 (DoS bound, TR-S-2): a history of exactly `MAX_HISTORY`
/// entries is served whole and recounted; one more and it is left out, the
/// client still checks the record and summary and reports `history_checked =
/// false`. The largest body a server can build (the full history plus a full
/// `chain`) stays under `MAX_BODY_BYTES`, the size the browser parses.
#[test]
fn the_history_cap_is_a_hard_bound_on_the_body() {
    let p = example::params();
    let (src, metric) = (SourceId(FORECASTER.into()), MetricId(METRIC.into()));
    let (b, c) = book_of(MAX_HISTORY);
    let full = Served {
        book: &b,
        commitment: &c,
        params: &p,
        chain: None,
    };
    let body = record_body(&full, &src, &metric).expect("body");
    let ok = check_record_body(&body, &expect(FORECASTER)).expect("full history checks");
    assert!(ok.history_checked);
    assert_eq!(ok.entries.len() as u64, MAX_HISTORY);
    let chain = vec![c.clone(); MAX_CHAIN];
    let worst = record_body(
        &Served {
            chain: Some(&chain),
            ..full
        },
        &src,
        &metric,
    )
    .expect("worst body");
    let size = serde_json::to_string(&worst).expect("json").len();
    assert!(size < MAX_BODY_BYTES, "worst body {size} bytes");

    let (b, c) = book_of(MAX_HISTORY + 1);
    let body = record_body(
        &Served {
            book: &b,
            commitment: &c,
            params: &p,
            chain: None,
        },
        &src,
        &metric,
    )
    .expect("body");
    assert_eq!(body["history"], Json::Null);
    let ok = check_record_body(&body, &expect(FORECASTER)).expect("record and summary check");
    assert!(!ok.history_checked && ok.entries.is_empty());
}
