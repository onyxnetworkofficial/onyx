//! Data submission intake (ROADMAP S1, ADR-0038, docs/spec/submissions.md).
//!
//! A person who wants to add data answers at most five questions
//! ([`Answers`]); [`classify`] turns them into a [`Route`]: the proof level the
//! data can reach, what happens next, and plain reasons. The details they type
//! ([`Submission`]) are checked with byte caps and https-only URLs, and carry no
//! e-mail or other personal contact field. [`fetch`] decides whether a server
//! may pull the named endpoint once to check it (only hosts already
//! rights-cleared in docs/ops/data-sources.md), which addresses it may connect
//! to, and what a preview of the pulled body shows.
//!
//! This crate is sector-agnostic and does no I/O: the API (`onyx-api`'s
//! `submit` module) does the fetch through a trait, and the website runs its
//! own copy of these rules (apps/web/src/lib/intake.ts). Both are pinned to the
//! same vectors, `fixtures/intake/vectors.json`, so the two cannot drift.
//!
//! Nothing here publishes anything. A submission is recorded for a reviewer
//! ("pending review"); the route is advice to the submitter and to the
//! reviewer, never a proof level granted to a value.

pub mod answers;
pub mod bounty;
pub mod fetch;
pub mod submission;

pub use answers::{
    classify, level_name, Answers, AnswersError, Changes, Kind, Outcome, Path, Reason, Rights,
    Route,
};
pub use fetch::{
    fetch_policy, is_blocked_host, is_public_ip, preview, FetchPolicy, NoFetch, Preview,
    PreviewValue, BLOCKED_HOSTS, CLEARED_HOSTS, FETCH_TIMEOUT_SECS, MAX_BODY_BYTES,
};
pub use submission::{
    DataPayload, HttpsUrl, KeyProof, Submission, SubmissionError, KEY_PLACEHOLDER,
    MAX_PAYLOAD_BYTES, STUB_PROVER,
};

/// The shared vectors' path from the repository root.
pub const VECTORS_PATH: &str = "fixtures/intake/vectors.json";
