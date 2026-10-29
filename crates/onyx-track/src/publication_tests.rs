//! Publications (ROADMAP C20, ADR-0036 amendment 5): one Borsh value that
//! carries every commitment of a chain with its logs, checked by a node before
//! serving and by every client after.

use super::*;
use crate::example::{self, AS_OF, AS_OF_EARLY};
use crate::{compute, ExtendError, Outcome};
use onyx_core::ProofLevel;

/// The example's chain as a coordinator publishes it: the early commitment,
/// then the latest.
pub(crate) fn example_publication() -> Publication {
    let (p, reg) = (example::params(), example::registry());
    let mut out = Publication::new();
    for as_of in [AS_OF_EARLY, AS_OF] {
        let (s, r) = example::logs(as_of);
        let book = compute(&p, &reg, &s, &r, as_of).expect("compute");
        out.push(&p, &s, &r, &book, None).expect("push");
    }
    out
}

#[test]
fn a_publication_carries_the_example_chain_and_checks() {
    let pubn = example_publication();
    let [(c1, _), (c2, latest)] = example::chain().expect("chain");
    assert_eq!(pubn.steps.len(), 2);
    assert_eq!(pubn.steps[0].commitment, c1);
    assert_eq!(pubn.steps[1].commitment, c2);
    let book = pubn.check().expect("checks");
    assert_eq!(book.commitment(c1.hash()), c2);
    assert_eq!(book.entries(), latest.entries());
    // Each step's log is a prefix of the publication-order logs, sorted.
    for (i, as_of) in [AS_OF_EARLY, AS_OF].into_iter().enumerate() {
        let (s, r) = example::logs(as_of);
        assert_eq!(
            step_log(&pubn.steps[i], &pubn.statements, &pubn.resolutions),
            Some((s, r))
        );
    }
    let bytes = borsh::to_vec(&pubn).expect("borsh");
    let back: Publication = borsh::from_slice(&bytes).expect("borsh");
    assert_eq!(back, pubn);
    assert_eq!(pubn.priced_answers(), 0, "the example resolves at level 3");
}

/// TR-S-1 at the source: a coordinator cannot append a computation that
/// drops a committed statement.
#[test]
fn push_refuses_a_computation_that_drops_a_statement() {
    let (p, reg) = (example::params(), example::registry());
    let mut pubn = Publication::new();
    let (s1, r1) = example::logs(AS_OF_EARLY);
    let b1 = compute(&p, &reg, &s1, &r1, AS_OF_EARLY).expect("compute");
    pubn.push(&p, &s1, &r1, &b1, None).expect("push");
    let (mut s2, r2) = example::logs(AS_OF);
    let dropped = s1[0].clone();
    s2.retain(|s| *s != dropped);
    let b2 = compute(&p, &reg, &s2, &r2, AS_OF).expect("compute");
    assert_eq!(
        pubn.push(&p, &s2, &r2, &b2, None),
        Err(PublicationError::Extend(1, ExtendError::StatementDropped))
    );
    // Inputs that are not the book's are refused before anything else.
    let (s, r) = example::logs(AS_OF);
    let b = compute(&p, &reg, &s, &r, AS_OF).expect("compute");
    assert_eq!(
        pubn.push(&p, &s1, &r, &b, None),
        Err(PublicationError::Inputs)
    );
}

/// A file whose logs were reordered so an earlier commitment's prefix is not
/// its log, whose counts fall, or whose rows do not give the latest
/// commitment, is refused by the node's check.
#[test]
fn a_tampered_publication_is_refused() {
    let good = example_publication();

    let mut moved = good.clone();
    let n = moved.statements.len();
    moved.statements.swap(0, n - 1);
    assert_eq!(moved.check().err(), Some(PublicationError::Log(0)));

    let mut counts = good.clone();
    counts.steps[1].statements = counts.steps[0].statements - 1;
    assert!(matches!(
        counts.check().err(),
        Some(PublicationError::Counts(_))
    ));

    let mut extra = good.clone();
    extra.statements.push(extra.statements[0].clone());
    assert!(matches!(
        extra.check().err(),
        Some(PublicationError::Counts(_))
    ));

    let mut rows = good.clone();
    let e = rows
        .entries
        .iter_mut()
        .find(|e| e.outcome == Outcome::Disagreed)
        .expect("a miss");
    e.outcome = Outcome::Agreed;
    assert_eq!(rows.check().err(), Some(PublicationError::Rows));

    let mut first = good.clone();
    first.steps.remove(0);
    assert_eq!(first.check().err(), Some(PublicationError::NotFirst));

    // Versions 1 and 2 are the two layouts (ADR-0036 amendments 5, 6);
    // anything else is refused, and a version-1 file never holds chained logs
    // or a registry.
    let mut version = good.clone();
    version.version = 3;
    assert_eq!(version.check().err(), Some(PublicationError::Version(3)));
    let mut format = good.clone();
    format.format = crate::chained::LOG_FORMAT_CHAINED;
    assert_eq!(format.check().err(), Some(PublicationError::Format(2)));
    assert!(borsh::to_vec(&format).is_err());

    let mut intake = good;
    intake.intake = Some(crate::intake::PublishedIntake {
        intake: Hash32([1; 32]),
        head: crate::intake::Anchor {
            block: 1,
            block_hash: Hash32([2; 32]),
            timestamp: AS_OF,
        },
        posts: vec![],
        batches: vec![],
        books: vec![],
    });
    assert_eq!(intake.check().err(), Some(PublicationError::Intake));
}

/// A node only takes a file that starts with what it served.
#[test]
fn a_later_file_must_start_with_the_earlier_one() {
    let full = example_publication();
    let mut early = full.clone();
    early.steps.truncate(1);
    early
        .statements
        .truncate(early.steps[0].statements as usize);
    early
        .resolutions
        .truncate(early.steps[0].resolutions as usize);
    assert!(full.starts_with(&early));
    assert!(full.starts_with(&full));
    assert!(!early.starts_with(&full));
    let mut rewritten = full.clone();
    rewritten.statements.swap(0, 1);
    assert!(!rewritten.starts_with(&early));
}

/// Level 1 and 2 answers are priced (amendment 5 item 2); level 3 is free.
#[test]
fn priced_answers_are_level_1_and_2() {
    let (_, r) = example::logs(AS_OF);
    let mut rs = r[..3].to_vec();
    assert_eq!(priced_answers(&rs), 0);
    rs[0].level = ProofLevel::Recomputed;
    assert_eq!(priced_answers(&rs), 1);
    for l in [
        ProofLevel::Recomputed,
        ProofLevel::ReObserved,
        ProofLevel::SourceProven,
    ] {
        rs[2].level = l;
        let want = 1 + usize::from(matches!(l.number(), 1 | 2));
        assert_eq!(priced_answers(&rs), want, "{l:?}");
    }
}

/// ROADMAP C21/C23 (ADR-0036 amendment 6 item 2): a version-1 file is the C20
/// layout byte for byte (version, then the steps); the second layout carries
/// its format byte and the registry and reads back equal.
#[test]
fn both_layouts_round_trip_and_the_first_is_unchanged() {
    let v1 = example_publication();
    let bytes = borsh::to_vec(&v1).expect("borsh");
    assert_eq!(bytes[0], PUBLICATION_VERSION);
    assert_eq!(
        &bytes[1..5],
        &2u32.to_le_bytes(),
        "steps follow the version"
    );
    let (p, reg) = (example::params(), example::registry());
    let mut v2 = Publication::chained();
    for as_of in [AS_OF_EARLY, AS_OF] {
        let (s, r) = example::logs(as_of);
        let book = compute(&p, &reg, &s, &r, as_of).expect("compute");
        v2.push_with(&p, &s, &r, &book, None, Some(&reg))
            .expect("push");
    }
    let bytes = borsh::to_vec(&v2).expect("borsh");
    assert_eq!(
        (bytes[0], bytes[1]),
        (PUBLICATION_VERSION_2, crate::chained::LOG_FORMAT_CHAINED)
    );
    let back: Publication = borsh::from_slice(&bytes).expect("decode");
    assert_eq!(back, v2);
    assert!(back.registry.is_some());
    assert!(borsh::from_slice::<Publication>(&[3]).is_err());
}

/// TR-C21-9: a chained push whose computation drops a committed statement
/// (the row would stay in the publication but not in the book) is refused.
#[test]
fn a_chained_push_refuses_a_computation_that_drops_a_row() {
    let (p, reg) = (example::params(), example::registry());
    let mut pubn = Publication::chained();
    let (s1, r1) = example::logs(AS_OF_EARLY);
    let b1 = compute(&p, &reg, &s1, &r1, AS_OF_EARLY).expect("compute");
    pubn.push(&p, &s1, &r1, &b1, None).expect("push");
    let (mut s2, r2) = example::logs(AS_OF);
    s2.retain(|s| *s != s1[0]);
    let b2 = compute(&p, &reg, &s2, &r2, AS_OF).expect("compute");
    assert_eq!(
        pubn.push(&p, &s2, &r2, &b2, None).err(),
        Some(PublicationError::Extend(1, ExtendError::StatementDropped))
    );
    let (s2, mut r2) = example::logs(AS_OF);
    r2.retain(|r| *r != r1[0]);
    let b2 = compute(&p, &reg, &s2, &r2, AS_OF).expect("compute");
    assert_eq!(
        pubn.push(&p, &s2, &r2, &b2, None).err(),
        Some(PublicationError::Extend(1, ExtendError::ResolutionDropped))
    );
    assert_eq!(pubn.steps.len(), 1, "nothing appended");
}
