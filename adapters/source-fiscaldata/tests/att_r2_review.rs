//! ATT-R2 (post-merge adversarial review of the ATT-R1 fixes): the review's
//! proving tests, turned into checks of the fixed behaviour (F1: the website
//! summary seals a round without a value; F2: salts are each observer's own
//! random secret; F3: a run that fails after its beacon is fixed still
//! publishes its round).

use onyx_api::observations::{ObservationStore, TemplatePins};
use onyx_coordinator::observe::{ObservationOutcome, SimulatedRegistry};
use onyx_core::{keccak256, observation_commit, observe, Answer, Hash32, Observation};
use onyx_source_fiscaldata::round::{
    append_attempt, append_results, results_store, run, store, summary, Page,
};
use onyx_source_fiscaldata::{
    recorded, subject, template, FiscalDataFetcher, RECORDED_BODY, RECORDED_WINDOW_START,
};

const NOW: u64 = RECORDED_WINDOW_START + 86_400;

fn hx(b: &[u8]) -> String {
    let mut s = String::from("0x");
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// One observer gets the page, two fail: `SourceUnavailable`, and the one
/// value reveal carries the record's value.
fn partial_outage() -> (String, Vec<Page>, Vec<String>) {
    let rec = recorded();
    let pages = rec
        .fetches
        .iter()
        .enumerate()
        .map(|(i, f)| Page {
            fetched_unix: f.fetched_unix,
            body: (i == 0).then(|| f.body.clone()),
        })
        .collect();
    let dates = rec.fetches.iter().map(|f| f.http_date.clone()).collect();
    (rec.record_date, pages, dates)
}

/// ATT-R2-F1: the website summary of a round without a value withholds what
/// the free API answer seals (salts, page hashes, the raw page), and keeps the
/// signed commits. ATT-R2-F2: the salts are the observers' own random
/// secrets, so no sealed commit opens by search over the window's seconds
/// with a salt computed from the public round id.
#[test]
fn a_sealed_round_stays_sealed_on_the_site_and_its_salts_are_private() {
    let mut chain = SimulatedRegistry::new();
    let (date, pages, dates) = partial_outage();
    let mut s = store();
    let r = run(&date, RECORDED_WINDOW_START, &pages, &mut s, &mut chain).expect("runs");
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::SourceUnavailable
    );
    let pins = TemplatePins::from([(template().metric.0.clone(), template().id())]);
    let feed =
        ObservationStore::from_json(&results_store(&r).expect("store").to_string(), NOW, &pins)
            .expect("loads");
    let fact = feed.attempt_fact(&subject(&date), None).expect("indexed");
    assert!(!feed.fact_closed(&fact));
    let free = feed.attempts_body(&feed.attempts_of(&fact), false);
    let free_text = free.to_string();
    let (_, value_obs, value_salt, _) = &r.reveals[0];
    assert!(matches!(value_obs.answer, Answer::Number(_)));
    assert!(
        !free_text.contains(&hx(&value_salt.0)[2..]),
        "API seals the salt"
    );

    // F1: the website summary of the round is sealed the same way.
    let site_json = summary(&r, &date, &dates, RECORDED_BODY, r.claim.is_none());
    let site = site_json.to_string();
    for (what, secret) in [
        ("salt", hx(&value_salt.0)),
        ("page hash", hx(&value_obs.page_hash.0)),
    ] {
        assert!(!site.contains(&secret[2..]), "summary carries the {what}");
    }
    assert!(
        !site.contains("response_body"),
        "summary carries the raw page"
    );
    assert!(site_json["reveals"].is_null());
    assert_eq!(site_json["sealed"], true);
    assert_eq!(
        site_json["commits"].as_array().map(Vec::len),
        Some(r.commits.len()),
        "the signed commits stay"
    );
    assert_eq!(site_json["value"], "", "no value shown");

    // F2: no sealed commit opens with the old public salt rule.
    let a = &free["attempts"][0];
    let round_id: Hash32 = {
        let h = a["roundId"].as_str().expect("roundId");
        let h = h.strip_prefix("0x").unwrap_or(h);
        let mut b = [0u8; 32];
        for (i, c) in b.iter_mut().enumerate() {
            *c = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).expect("hex");
        }
        Hash32(b)
    };
    let mut opened = 0;
    for (k, o) in r.observers.iter().enumerate() {
        let i = u8::try_from(k + 1).expect("small");
        let salt = keccak256(&[b"onyx-l2-rehearsal-salt".as_slice(), &round_id.0, &[i]].concat());
        let commit = r.attempt_commit(&o.address);
        for t in r.spec.window_start..=r.spec.window_end {
            let unavailable = Observation {
                answer: Answer::Unavailable,
                observed_at: t,
                page_hash: Hash32::ZERO,
            };
            let from_page =
                observe(&FiscalDataFetcher, &r.spec, Some(RECORDED_BODY), t).expect("observe");
            if [unavailable, from_page]
                .iter()
                .any(|ob| observation_commit(&round_id, &o.address, ob, &salt) == commit)
            {
                opened += 1;
                break;
            }
        }
    }
    assert_eq!(opened, 0, "no sealed commit opens with a public salt");
    // Each run draws fresh salts: the same round replayed commits otherwise.
    let mut chain2 = SimulatedRegistry::new();
    let r2 = run(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut store(),
        &mut chain2,
    )
    .expect("runs");
    assert_eq!(r2.round_id, r.round_id, "the same round");
    assert_ne!(r2.reveals[0].2, r.reveals[0].2, "fresh salts");
}

trait CommitOf {
    fn attempt_commit(&self, a: &[u8; 20]) -> Hash32;
}

impl CommitOf for onyx_source_fiscaldata::round::RoundRun {
    fn attempt_commit(&self, a: &[u8; 20]) -> Hash32 {
        onyx_source_fiscaldata::round::attempt(self)
            .commits
            .iter()
            .find(|(x, _, _)| x == a)
            .map(|(_, c, _)| *c)
            .expect("committed")
    }
}

use onyx_api::client::{verify_observation, ObservationExpect};
use onyx_coordinator::observe::{ChainView, DrawChain, DrawRecord, OnchainDraw, OnchainFact};

/// A chain that keeps every draw record `fix_beacon` returned.
struct Recording {
    inner: SimulatedRegistry,
    fixed: Vec<DrawRecord>,
}

impl ChainView for Recording {
    fn height(&self) -> Result<u64, String> {
        self.inner.height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.inner.block_hash(h)
    }
}

impl DrawChain for Recording {
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        self.inner.draw_count(f)
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        self.inner.draw_budget(f)
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        self.inner.announce(d)
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        let r = self.inner.fix_beacon(d)?;
        self.fixed.push(r);
        Ok(r)
    }
    fn draw_of(&self, f: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String> {
        self.inner.draw_of(f, attempt)
    }
    fn draw_of_final(
        &self,
        f: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String> {
        self.inner.draw_of_final(f, attempt)
    }
    fn post_commit(
        &mut self,
        f: &OnchainFact,
        attempt: u32,
        o: &[u8; 20],
        round: &Hash32,
        commit: &Hash32,
        sig: &[u8; 65],
    ) -> Result<onyx_coordinator::observe::Posted, String> {
        self.inner.post_commit(f, attempt, o, round, commit, sig)
    }
    fn post_receipt(
        &mut self,
        f: &OnchainFact,
        attempt: u32,
        o: &[u8; 20],
        round: &Hash32,
        commit: &Hash32,
        sig: &[u8; 65],
    ) -> Result<onyx_coordinator::observe::Posted, String> {
        self.inner.post_receipt(f, attempt, o, round, commit, sig)
    }
    fn log_block(&self) -> Result<Option<onyx_coordinator::observe::LogBlock>, String> {
        self.inner.log_block()
    }
    fn posted_commit(
        &self,
        block: u64,
        f: &OnchainFact,
        attempt: u32,
        o: &[u8; 20],
        round: &Hash32,
    ) -> Result<Option<onyx_coordinator::observe::PostedCommit>, String> {
        self.inner.posted_commit(block, f, attempt, o, round)
    }
    fn transcript_complete(
        &self,
        q: &onyx_coordinator::observe::TranscriptLogQuery,
    ) -> Result<bool, String> {
        self.inner.transcript_complete(q)
    }
}

fn word(v: u64) -> Vec<u8> {
    let mut w = vec![0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

/// ATT-R2-F3: a run that fails AFTER `draw_and_open` fixed its beacon (here:
/// a page count mismatch) returns its opened round, the runner publishes it
/// as an attempt, and the honest value of the re-draw then passes
/// `verify_attempts_onchain` (before the fix the chain held a fixed draw with
/// no published round, and the fact failed for good).
#[test]
fn a_runner_error_after_the_beacon_still_publishes_the_round() {
    let mut chain = Recording {
        inner: SimulatedRegistry::new(),
        fixed: vec![],
    };
    let rec = recorded();
    let good: Vec<Page> = rec
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect();
    let mut s = store();
    let err = run(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &good[..2],
        &mut s,
        &mut chain,
    )
    .expect_err("the runner errs");
    assert_ne!(
        chain.fixed[0].beacon,
        Hash32::ZERO,
        "draw 0's beacon is fixed"
    );
    let failed = err.attempt.expect("the opened round is returned");
    assert_eq!(failed.draw.attempt, 0);
    let store0 = append_attempt(None, &failed).expect("published");
    let second = run(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &good,
        &mut s,
        &mut chain,
    )
    .expect("re-draw");
    assert_eq!(second.draw.attempt, 1);
    let fact = s.onchain_fact(&second.spec).expect("fact");
    chain.inner.record_value(&fact);
    let pins = TemplatePins::from([(template().metric.0.clone(), template().id())]);
    let feed = ObservationStore::from_json(
        &append_results(Some(&store0), &second)
            .expect("store")
            .to_string(),
        NOW,
        &pins,
    )
    .expect("loads");
    let r = feed
        .record(&subject(&rec.record_date), None, None)
        .expect("record");
    let all = feed.body_all(r, &feed.attempts_of(&r.checked.fact));
    let t = template();
    let e = ObservationExpect {
        template: &t,
        subject: &second.spec.subject,
        window_end: None,
        min_window_end: 0,
        now: NOW,
    };
    let a = verify_observation(&all, &e)
        .expect("verifies")
        .attempts
        .expect("all");
    assert_eq!(a.attempts.len(), 1, "the failed round is published");
    let d0 = &chain.fixed[0];
    let mut draw0 = d0.commit.0.to_vec();
    draw0.extend(word(d0.beacon_height));
    draw0.extend(word(d0.announced_at));
    draw0.extend_from_slice(&d0.beacon.0);
    let (strikes, allowance) = chain.inner.draw_budget(&fact).expect("budget");
    let mut budget = word(u64::from(strikes));
    budget.extend(word(u64::from(allowance)));
    let count = word(u64::from(chain.inner.draw_count(&fact).expect("count")));
    onyx_api::client::verify_attempts_onchain(
        &a,
        &count,
        &[draw0],
        &budget,
        &complete(&a),
        &unsealed_all(&a),
    )
    .expect("the honest value passes with the failed round published");
}

/// `transcriptComplete` returns of an untampered log: true for every
/// published round (ADR-0027 amendment 8).
fn complete(a: &onyx_api::client::VerifiedAttempts) -> Vec<Vec<u8>> {
    let mut yes = vec![0u8; 32];
    yes[31] = 1;
    vec![yes; a.attempts.len()]
}

/// ROADMAP C11: what the registry answers to each published round's sealed
/// calls when its draw has no reveal key (no sealed post) and the round is
/// recorded.
fn unsealed_all(a: &onyx_api::client::VerifiedAttempts) -> Vec<onyx_api::client::SealedRets> {
    a.attempts
        .iter()
        .map(|c| onyx_api::client::honest_sealed_returns(&c.reveal_log, None, true, true))
        .collect()
}
