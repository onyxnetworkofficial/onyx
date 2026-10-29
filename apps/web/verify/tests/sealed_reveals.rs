//! ROADMAP C11 in the browser: the page's mirrors of `onyx_coordinator::sealed`
//! match the coordinator, and a level-2 round carrying its sealed posts
//! (`sealed_reveals`) checks only when every post, dismissal and recorded chain
//! answer is the round's own: the drawn set, the config's window and the
//! recomputed round id.

use onyx_coordinator::sealed::{
    dismiss, key_proof, posted_returns, public_point, reveal_log_query, seal, seal_bytes,
    sealed_len as coord_sealed_len, valid_reveal, PostedSealed, RevealContext,
};
use onyx_core::{keccak256, Answer, Hash32, Observation};
use onyx_web_verify::check_level2_round;
use onyx_web_verify::sealed::{
    decrypt, dismissal_proof_checks, is_valid_reveal, key_proof_valid, posted_from_returns,
    sealed_len, LogQuery,
};
use serde_json::{json, Value};
use std::collections::BTreeSet;

const SUMMARY: &str = include_str!("../../static/level2/debt-to-penny.json");
/// The coordinator's reveal key in these tests (a fixed test scalar).
const REVEAL: [u8; 32] = [0x42; 32];

fn h(s: &str) -> Vec<u8> {
    hex::decode(s.trim_start_matches("0x")).expect("hex")
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn h32(s: &str) -> Hash32 {
    Hash32(h(s).try_into().expect("32 bytes"))
}

fn check(r: &Value) -> Value {
    serde_json::from_str(&check_level2_round(&r.to_string()).unwrap_or_else(|_| panic!("errored")))
        .expect("json")
}

fn spec(r: &Value) -> onyx_core::ObservationSpec {
    onyx_source_fiscaldata::spec(
        r["feed"]["record_date"].as_str().expect("date"),
        r["feed"]["window_start"].as_u64().expect("start"),
    )
    .expect("spec")
}

fn registry_fact(r: &Value) -> Hash32 {
    let t = onyx_source_fiscaldata::template();
    let sp = spec(r);
    onyx_core::onchain::registry_fact_key(
        &onyx_core::onchain::metric_key(&t.metric),
        &t.id(),
        &onyx_core::onchain::subject_key(&sp.subject),
        sp.window_end,
        t.is_dated(),
    )
}

/// The rehearsal key of the pool member `a` (public test keys).
fn key_of(a: &[u8; 20]) -> [u8; 32] {
    (1..=onyx_source_fiscaldata::round::POOL_SIZE)
        .find(|i| onyx_source_fiscaldata::round::observer_address(*i) == *a)
        .map(onyx_source_fiscaldata::round::observer_key)
        .expect("a rehearsal pool member")
}

fn addr(s: &str) -> [u8; 20] {
    h(s).try_into().expect("20 bytes")
}

/// The recorded round with an honest `sealed_reveals` section: each observer's
/// recorded reveal sealed to [`REVEAL`] and posted in the reveal phase, and the
/// chain's answers to the page's calls, recorded (built with the coordinator's
/// own code, not the page's mirrors).
fn sealed_summary() -> Value {
    let mut r: Value = serde_json::from_str(SUMMARY).expect("json");
    let sp = spec(&r);
    let rid = h32(r["round_id"].as_str().expect("round id"));
    let attempt = u32::try_from(r["draw"]["attempt"].as_u64().expect("attempt")).expect("u32");
    let cc = r["config"]["commit_close"].as_u64().expect("cc");
    let rc = r["config"]["reveal_close"].as_u64().expect("rc");
    let cents =
        onyx_source_fiscaldata::parse_cents(r["value"].as_str().expect("value")).expect("cents");
    let key = public_point(&REVEAL).expect("key");
    let mut posts = vec![];
    let mut chain_posts = vec![];
    let mut commits = vec![];
    for (i, e) in r["reveals"].as_array().expect("reveals").iter().enumerate() {
        let a = addr(e["observer"].as_str().expect("observer"));
        let o = Observation {
            answer: Answer::Number(cents),
            observed_at: e["observed_unix"].as_u64().expect("at"),
            page_hash: h32(e["page_hash"].as_str().expect("page")),
        };
        let salt = h32(e["salt"].as_str().expect("salt"));
        let p = seal(
            &key_of(&a),
            &keccak256(&[b'e', i as u8]).0,
            &key,
            &rid,
            attempt,
            &sp,
            &o,
            &salt,
        )
        .expect("seal");
        let posted = PostedSealed {
            ephemeral: p.ephemeral,
            key_proof: p.key_proof,
            posted_at: cc + 1 + i as u64,
            ciphertext: p.ciphertext.clone(),
        };
        let (rv, kp) = posted_returns(Some(&posted));
        posts.push(json!({
            "observer": hx(&a),
            "ephemeral": hx(&p.ephemeral),
            "key_proof": hx(&p.key_proof),
            "posted_at": posted.posted_at,
            "ciphertext": hx(&p.ciphertext),
        }));
        chain_posts.push(json!({
            "observer": hx(&a),
            "sealed_reveal_of": hx(&rv),
            "sealed_key_proof_of": hx(&kp),
        }));
        commits.push((a, h32(e["commit"].as_str().expect("commit")), [0u8; 65]));
    }
    let observers: Vec<onyx_coordinator::observe::Observer> = r["bonds"]
        .as_array()
        .expect("bonds")
        .iter()
        .map(|b| onyx_coordinator::observe::Observer {
            address: addr(b["observer"].as_str().expect("observer")),
            bond: b["bond"].as_str().expect("bond").parse().expect("bond"),
        })
        .collect();
    let revealed: BTreeSet<[u8; 20]> = commits.iter().map(|(a, _, _)| *a).collect();
    let cfg = onyx_coordinator::observe::ObserveConfig {
        commit_close: cc,
        reveal_close: rc,
        reward: 0,
        outlier_slash: 0,
        miss_slash: 0,
    };
    let q = reveal_log_query(
        registry_fact(&r),
        &sp,
        &cfg,
        attempt,
        rid,
        &observers,
        &commits,
        &revealed,
        &BTreeSet::new(),
    );
    r["sealed_reveals"] = json!({
        "reveal_key": hx(&key),
        "posts": posts,
        "dismissals": [],
        "record": null,
        "chain": {
            "draw_reveal_key": hx(&key),
            "reveal_log_complete": {"calldata": hx(&q.calldata()), "answer": true},
            "draw_time_locked": {
                "calldata": hx(&onyx_coordinator::sealed::draw_time_locked_calldata(
                    &registry_fact(&r),
                    attempt,
                )),
                "answer": false,
            },
            "posts": chain_posts,
        },
    });
    r
}

/// The page's mirrors are the coordinator's: key proofs, dismissal proofs,
/// decryption, the plaintext rule, the query's calldata and digest, and the
/// strict ABI reader.
#[test]
fn c11_the_page_mirrors_match_the_coordinator() {
    let r: Value = serde_json::from_str(SUMMARY).expect("json");
    let sp = spec(&r);
    assert_eq!(sealed_len(&sp), coord_sealed_len(&sp));
    let key = public_point(&REVEAL).expect("key");
    for i in 0..16u8 {
        let round = keccak256(&[b'r', i]);
        let obs = keccak256(&[b'o', i]).0;
        let me = onyx_labels::attest::secret_address(&obs).expect("key");
        let eph = keccak256(&[b'e', i]).0;
        let a = u32::from(i);
        let p = seal_bytes(&obs, &me, &eph, &key, &round, a, &[i; 154]).expect("seal");
        assert_eq!(p.key_proof, key_proof(&eph, &round, a, &me).expect("proof"));
        assert!(key_proof_valid(&p.ephemeral, &round, a, &me, &p.key_proof));
        assert!(!key_proof_valid(
            &p.ephemeral,
            &round,
            a + 1,
            &me,
            &p.key_proof
        ));
        let d = dismiss(&REVEAL, &p, &round, a).expect("dismiss");
        assert!(dismissal_proof_checks(
            &key,
            &p.ephemeral,
            &round,
            a,
            &me,
            &d.shared,
            &d.proof
        ));
        assert!(!dismissal_proof_checks(
            &key,
            &p.ephemeral,
            &keccak256(b"other round"),
            a,
            &me,
            &d.shared,
            &d.proof
        ));
        assert_eq!(
            decrypt(&d.shared, &p.ephemeral, &round, a, &me, &p.ciphertext),
            vec![i; 154]
        );
        let posted = PostedSealed {
            ephemeral: p.ephemeral,
            key_proof: p.key_proof,
            posted_at: 1_000 + u64::from(i),
            ciphertext: p.ciphertext.clone(),
        };
        let (rv, kp) = posted_returns(Some(&posted));
        assert_eq!(
            posted_from_returns(&rv, &kp),
            Some((
                p.ephemeral,
                p.ciphertext.clone(),
                p.key_proof,
                posted.posted_at
            ))
        );
        let (e0, k0) = posted_returns(None);
        assert_eq!(posted_from_returns(&e0, &k0), None);
    }
    // The plaintext rule, on a real reveal of the recorded round.
    let s = sealed_summary();
    let rid = h32(s["round_id"].as_str().expect("rid"));
    let attempt = s["draw"]["attempt"].as_u64().expect("a") as u32;
    let p0 = &s["sealed_reveals"]["posts"][0];
    let a = addr(p0["observer"].as_str().expect("observer"));
    let e: [u8; 64] = h(p0["ephemeral"].as_str().expect("e"))
        .try_into()
        .expect("64");
    let ct = h(p0["ciphertext"].as_str().expect("ct"));
    let shared = {
        let post = onyx_coordinator::sealed::SealedPost {
            observer: a,
            ephemeral: e,
            ciphertext: ct.clone(),
            key_proof: [0; 52],
            sig: [0; 65],
        };
        dismiss(&REVEAL, &post, &rid, attempt).expect("S").shared
    };
    let plain = decrypt(&shared, &e, &rid, attempt, &a, &ct);
    let commit = h32(s["reveals"][0]["commit"].as_str().expect("commit"));
    let ctx = RevealContext {
        round_id: rid,
        attempt,
        spec: &sp,
        commit: Some(commit),
    };
    assert!(valid_reveal(&plain, &a, &ctx).is_some());
    assert!(is_valid_reveal(
        &plain,
        &a,
        &sp,
        &rid,
        attempt,
        Some(commit)
    ));
    assert!(!is_valid_reveal(
        &plain,
        &a,
        &sp,
        &rid,
        attempt,
        Some(Hash32([1; 32]))
    ));
    assert!(!is_valid_reveal(
        &plain,
        &a,
        &sp,
        &rid,
        attempt + 1,
        Some(commit)
    ));
    // The query: calldata and digest.
    let q = LogQuery {
        registry_fact: keccak256(b"fact"),
        attempt: 2,
        round_id: rid,
        commit_open: 10,
        commit_close: 20,
        reveal_close: 30,
        entries: vec![([1; 20], Hash32([2; 32]), 2), ([3; 20], Hash32::ZERO, 0)],
    };
    let c = onyx_coordinator::sealed::RevealLogQuery {
        registry_fact: q.registry_fact,
        attempt: 2,
        round_id: rid,
        commit_open: 10,
        commit_close: 20,
        reveal_close: 30,
        entries: vec![
            onyx_coordinator::sealed::SealedEntry {
                observer: [1; 20],
                commit: Hash32([2; 32]),
                status: onyx_coordinator::sealed::EntryStatus::Dismissed,
            },
            onyx_coordinator::sealed::SealedEntry {
                observer: [3; 20],
                commit: Hash32::ZERO,
                status: onyx_coordinator::sealed::EntryStatus::NotRevealed,
            },
        ],
    };
    assert_eq!(q.reveal_log_calldata(), c.calldata());
    let res = keccak256(b"result");
    assert_eq!(q.matches_calldata(&res), c.matches_calldata(&res));
    assert_eq!(q.digest(&res), c.digest(&res));
}

/// The recorded round with its sealed posts checks; without the section it
/// checks as before (a draw without a reveal key).
#[test]
fn c11_a_round_with_its_sealed_posts_checks() {
    let plain: Value = serde_json::from_str(SUMMARY).expect("json");
    let out = check(&plain);
    assert_eq!(out["ok"], Value::Bool(true), "{out}");
    assert_eq!(out["sealed_reveals"], Value::Null);
    let s = sealed_summary();
    let out = check(&s);
    assert_eq!(out["ok"], Value::Bool(true), "{out}");
    assert_eq!(out["sealed_reveals"]["posts"], 5);
    assert_eq!(out["sealed_reveals"]["dismissed"], 0);
    assert_eq!(
        out["sealed_reveals"]["reveal_log_complete"],
        s["sealed_reveals"]["chain"]["reveal_log_complete"]["calldata"]
    );
}

fn refused(r: &Value, what: &str) {
    let out = check(r);
    assert_eq!(out["ok"], Value::Bool(false), "{what}: {out}");
    assert!(
        out["problems"]
            .as_array()
            .expect("problems")
            .iter()
            .any(|p| p.as_str().unwrap_or_default().contains("sealed reveals")),
        "{what}: {out}"
    );
}

/// Every way a served section can lie is refused: a false dismissal of a
/// valid post, a key proof that fails, a post outside the reveal phase, a post
/// that is not the chain's, chain answers for another window or a false one,
/// another reveal key, and a record on a value round.
#[test]
fn c11_the_page_refuses_a_section_that_is_not_the_rounds() {
    let s = sealed_summary();
    let rid = h32(s["round_id"].as_str().expect("rid"));
    let attempt = s["draw"]["attempt"].as_u64().expect("a") as u32;
    // A false dismissal: a real proof for a valid post (its plaintext opens
    // the commit). The page also needs the observer not revealed: it is, so
    // two problems.
    let p0 = s["sealed_reveals"]["posts"][0].clone();
    let post = onyx_coordinator::sealed::SealedPost {
        observer: addr(p0["observer"].as_str().expect("o")),
        ephemeral: h(p0["ephemeral"].as_str().expect("e"))
            .try_into()
            .expect("64"),
        ciphertext: h(p0["ciphertext"].as_str().expect("ct")),
        key_proof: [0; 52],
        sig: [0; 65],
    };
    let d = dismiss(&REVEAL, &post, &rid, attempt).expect("proof");
    let mut r = s.clone();
    r["sealed_reveals"]["dismissals"] = json!([{
        "observer": p0["observer"],
        "shared": hx(&d.shared),
        "proof": hx(&d.proof),
    }]);
    refused(&r, "false dismissal");
    let out = check(&r);
    assert!(out["problems"]
        .as_array()
        .expect("problems")
        .iter()
        .any(|p| p.as_str().unwrap_or_default().contains("is false")));
    // A key proof that fails.
    let mut r = s.clone();
    let mut kp = h(p0["key_proof"].as_str().expect("kp"));
    kp[51] ^= 1;
    r["sealed_reveals"]["posts"][0]["key_proof"] = json!(hx(&kp));
    refused(&r, "key proof");
    // A post outside the reveal phase.
    let mut r = s.clone();
    r["sealed_reveals"]["posts"][0]["posted_at"] = json!(r["config"]["commit_close"]);
    refused(&r, "early post");
    // A post that is not the chain's (the server's own ciphertext).
    let mut r = s.clone();
    let mut ct = h(p0["ciphertext"].as_str().expect("ct"));
    ct[0] ^= 1;
    r["sealed_reveals"]["posts"][0]["ciphertext"] = json!(hx(&ct));
    refused(&r, "not the chain's post");
    // The chain's answer for another window (reveal close moved).
    let mut r = s.clone();
    let mut cd = h(
        r["sealed_reveals"]["chain"]["reveal_log_complete"]["calldata"]
            .as_str()
            .expect("calldata"),
    );
    cd[4 + 6 * 32 - 1] ^= 1;
    r["sealed_reveals"]["chain"]["reveal_log_complete"]["calldata"] = json!(hx(&cd));
    refused(&r, "another window");
    // A false answer.
    let mut r = s.clone();
    r["sealed_reveals"]["chain"]["reveal_log_complete"]["answer"] = json!(false);
    refused(&r, "revealLogComplete false");
    // Another reveal key than the chain's.
    let mut r = s.clone();
    r["sealed_reveals"]["chain"]["draw_reveal_key"] =
        json!(hx(&public_point(&[0x43; 32]).expect("point")));
    refused(&r, "drawRevealKey");
    // Review C11-R1-3: a revealed observer without an in-phase post (a keyed
    // draw is sealed only).
    let mut r = s.clone();
    r["sealed_reveals"]["posts"]
        .as_array_mut()
        .expect("posts")
        .remove(0);
    r["sealed_reveals"]["chain"]["posts"]
        .as_array_mut()
        .expect("posts")
        .remove(0);
    refused(&r, "revealed without a post");
    // A value round with an attempt record.
    let mut r = s.clone();
    r["sealed_reveals"]["record"] = json!(hx(&[9; 32]));
    refused(&r, "record on a value round");
}

// ------------------------------------------------------------ ROADMAP C51

const BEACONS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/tlock/drand/quicknet-c51-rounds.json"
));

fn beacons() -> onyx_coordinator::timelock::FixtureBeacon {
    onyx_coordinator::timelock::FixtureBeacon::from_json(BEACONS).expect("fixture")
}

fn sig_of(round: u64) -> Vec<u8> {
    onyx_coordinator::timelock::BeaconSource::signature(&beacons(), round)
        .expect("read")
        .expect("a published round in the fixture")
}

fn lock_secrets(i: u8) -> onyx_coordinator::timelock::LockSecrets {
    let x = keccak256(&[b'x', i]).0;
    let mut tau = [0u8; 16];
    let mut sigma = [0u8; 16];
    tau.copy_from_slice(&x[..16]);
    sigma.copy_from_slice(&x[16..]);
    let mut kappa = [0u8; 16];
    kappa.copy_from_slice(&keccak256(&[b'k', i]).0[..16]);
    onyx_coordinator::timelock::LockSecrets {
        ephemeral: keccak256(&[b'e', i]).0,
        tau,
        sigma,
        kappa,
    }
}

/// The lock and far rounds of the recorded window (ROADMAP C52: the far round
/// from the fiscaldata template, with the coordinator's own code).
fn rounds_of(r: &Value, reveal_close: u64) -> onyx_coordinator::timelock::LockRounds {
    onyx_coordinator::timelock::LockRounds::of_round(
        &onyx_coordinator::timelock::BeaconChain::quicknet(),
        &onyx_source_fiscaldata::template(),
        &spec(r),
        reveal_close,
    )
    .expect("rounds")
}

/// The recorded round with a TIME-LOCKED `sealed_reveals` section: each
/// observer's recorded reveal sealed with drand/tlock's real IBE to the lock
/// round of the window, posted in the reveal phase, with the lock round's
/// published signature and the chain's `drawTimeLocked` true (built with the
/// coordinator's own code).
fn timelocked_summary() -> Value {
    use onyx_coordinator::timelock::{seal_timelocked, QuicknetTimeLock};
    let mut r = sealed_summary();
    let sp = spec(&r);
    let rid = h32(r["round_id"].as_str().expect("round id"));
    let attempt = u32::try_from(r["draw"]["attempt"].as_u64().expect("attempt")).expect("u32");
    let cc = r["config"]["commit_close"].as_u64().expect("cc");
    let rc = r["config"]["reveal_close"].as_u64().expect("rc");
    let cents =
        onyx_source_fiscaldata::parse_cents(r["value"].as_str().expect("value")).expect("cents");
    let key = public_point(&REVEAL).expect("key");
    let q = QuicknetTimeLock::pinned().expect("pinned");
    let rho = onyx_web_verify::sealed::lock_round(rc).expect("rho");
    let rounds = rounds_of(&r, rc);
    let mut posts = vec![];
    let mut chain_posts = vec![];
    for (i, e) in r["reveals"].as_array().expect("reveals").iter().enumerate() {
        let a = addr(e["observer"].as_str().expect("observer"));
        let o = Observation {
            answer: Answer::Number(cents),
            observed_at: e["observed_unix"].as_u64().expect("at"),
            page_hash: h32(e["page_hash"].as_str().expect("page")),
        };
        let salt = h32(e["salt"].as_str().expect("salt"));
        let p = seal_timelocked(
            &q,
            &key_of(&a),
            &lock_secrets(i as u8),
            &key,
            &rid,
            attempt,
            rounds,
            cc,
            &sp,
            &o,
            &salt,
        )
        .expect("seal");
        let posted = PostedSealed {
            ephemeral: p.ephemeral,
            key_proof: p.key_proof,
            posted_at: cc + 1 + i as u64,
            ciphertext: p.ciphertext.clone(),
        };
        let (rv, kp) = posted_returns(Some(&posted));
        posts.push(json!({
            "observer": hx(&a),
            "ephemeral": hx(&p.ephemeral),
            "key_proof": hx(&p.key_proof),
            "posted_at": posted.posted_at,
            "ciphertext": hx(&p.ciphertext),
        }));
        chain_posts.push(json!({
            "observer": hx(&a),
            "sealed_reveal_of": hx(&rv),
            "sealed_key_proof_of": hx(&kp),
        }));
    }
    r["sealed_reveals"]["posts"] = json!(posts);
    r["sealed_reveals"]["chain"]["posts"] = json!(chain_posts);
    r["sealed_reveals"]["chain"]["draw_time_locked"]["answer"] = json!(true);
    r["sealed_reveals"]["time_lock"] = json!({
        "lock_round": rho,
        "far_round": rounds.far(),
        "beacon": hx(&sig_of(rho)),
        "openings": [],
    });
    r
}

/// The page's time-lock mirrors are the coordinator's: the lock round, the
/// body key, and the dismissal rule on real time-locks (junk behind a valid
/// time-lock: the opening re-encrypts and the body is no reveal; junk with no
/// valid time-lock and a post locked to another round: they do not open under
/// the verified signature; a changed or dropped opening is refused).
#[test]
fn c51_the_page_time_lock_mirrors_match_the_coordinator() {
    use onyx_coordinator::timelock::{
        check_timelocked_dismissal, open_timelocked, seal_bytes_timelocked, BeaconChain,
        QuicknetTimeLock, TimedOpened,
    };
    use onyx_web_verify::sealed::{check_timelocked, lock_round};
    let r: Value = serde_json::from_str(SUMMARY).expect("json");
    let sp = spec(&r);
    let rc = r["config"]["reveal_close"].as_u64().expect("rc");
    let cc = r["config"]["commit_close"].as_u64().expect("cc");
    for t in [0u64, 1, 1_692_803_367, 1_791_696_840, 1_800_000_001] {
        assert_eq!(
            lock_round(t),
            onyx_coordinator::timelock::lock_round(&BeaconChain::quicknet(), t)
        );
    }
    let q = QuicknetTimeLock::pinned().expect("pinned");
    let b = onyx_tlock::BeaconKey::quicknet().expect("key");
    let rho = lock_round(rc).expect("rho");
    let sig = sig_of(rho);
    let key = public_point(&REVEAL).expect("key");
    let rid = keccak256(b"c51 page round");
    let rounds = rounds_of(&r, rc);
    for i in 0..4u8 {
        let obs = keccak256(&[b'o', i]).0;
        let me = onyx_labels::attest::secret_address(&obs).expect("key");
        let junk = vec![i; coord_sealed_len(&sp)];
        let p = match i {
            // Junk behind a valid time-lock.
            0 => seal_bytes_timelocked(
                &q,
                &obs,
                &me,
                &lock_secrets(i),
                &key,
                &rid,
                0,
                rounds,
                cc,
                &junk,
            )
            .expect("seal"),
            // Junk with no valid time-lock.
            1 => seal_bytes(
                &obs,
                &me,
                &keccak256(&[b'e', i]).0,
                &key,
                &rid,
                0,
                &[9u8; 426],
            )
            .expect("seal"),
            // Locked to an earlier round.
            2 => seal_bytes_timelocked(
                &q,
                &obs,
                &me,
                &lock_secrets(i),
                &key,
                &rid,
                0,
                rounds_of(&r, rc - 30),
                cc - 60,
                &junk,
            )
            .expect("seal"),
            // Too short to hold a time-lock and a body.
            _ => seal_bytes(
                &obs,
                &me,
                &keccak256(&[b'e', i]).0,
                &key,
                &rid,
                0,
                &[9u8; 100],
            )
            .expect("seal"),
        };
        let posted = PostedSealed {
            ephemeral: p.ephemeral,
            key_proof: p.key_proof,
            posted_at: cc + 1,
            ciphertext: p.ciphertext.clone(),
        };
        let ctx = RevealContext {
            round_id: rid,
            attempt: 0,
            spec: &sp,
            commit: Some(Hash32([7; 32])),
        };
        let Ok(TimedOpened::Dismissed(d)) =
            open_timelocked(&q, &REVEAL, Some(&sig), &me, &posted, &ctx, rounds)
        else {
            panic!("garbage {i} is dismissed");
        };
        assert_eq!(
            d.opening.is_some(),
            i == 0,
            "only junk behind a valid lock opens"
        );
        let want = check_timelocked_dismissal(&q, &key, &me, &posted, &ctx, rounds, Some(&sig), &d)
            .expect("the coordinator's check");
        let (page, _) = check_timelocked(
            &b,
            rho,
            rounds.far(),
            &sig,
            d.opening.map(|o| (o.sigma, o.secret)),
            &d.dismissal.shared,
            &p.ephemeral,
            &rid,
            0,
            &me,
            &p.ciphertext,
        )
        .expect("the page's check");
        assert_eq!(page, want, "garbage {i}: the same dismissed body");
        if let Some(o) = d.opening {
            let mut tau = o.secret;
            tau[0] ^= 1;
            assert!(check_timelocked(
                &b,
                rho,
                rounds.far(),
                &sig,
                Some((o.sigma, tau)),
                &d.dismissal.shared,
                &p.ephemeral,
                &rid,
                0,
                &me,
                &p.ciphertext
            )
            .is_err());
            assert!(
                check_timelocked(
                    &b,
                    rho,
                    rounds.far(),
                    &sig,
                    None,
                    &d.dismissal.shared,
                    &p.ephemeral,
                    &rid,
                    0,
                    &me,
                    &p.ciphertext
                )
                .is_err(),
                "a time-lock that opens needs its opening"
            );
        }
    }
}

/// A time-locked section checks; each lie is refused: another round's
/// signature, a forged one, another lock round, the time-lock hidden (the
/// chain's drawTimeLocked says true), the chain's answer for a draw that is not
/// time-locked, and a post without room for a time-lock.
#[test]
fn c51_a_time_locked_section_checks_and_every_lie_is_refused() {
    let s = timelocked_summary();
    let out = check(&s);
    assert_eq!(out["ok"], Value::Bool(true), "{out}");
    assert_eq!(out["sealed_reveals"]["time_locked"], Value::Bool(true));
    let rho = s["sealed_reveals"]["time_lock"]["lock_round"]
        .as_u64()
        .expect("rho");
    assert_eq!(out["sealed_reveals"]["lock_round"], rho);
    let mut r = s.clone();
    r["sealed_reveals"]["time_lock"]["beacon"] = json!(hx(&sig_of(rho + 1)));
    refused(&r, "another round's signature");
    let mut r = s.clone();
    let mut sig = sig_of(rho);
    sig[47] ^= 1;
    r["sealed_reveals"]["time_lock"]["beacon"] = json!(hx(&sig));
    refused(&r, "a forged signature");
    let mut r = s.clone();
    r["sealed_reveals"]["time_lock"]["lock_round"] = json!(rho + 1);
    r["sealed_reveals"]["time_lock"]["beacon"] = json!(hx(&sig_of(rho + 1)));
    refused(&r, "another lock round");
    let mut r = s.clone();
    r["sealed_reveals"]
        .as_object_mut()
        .expect("section")
        .remove("time_lock");
    refused(&r, "the time-lock hidden");
    let mut r = s.clone();
    r["sealed_reveals"]["chain"]["draw_time_locked"]["answer"] = json!(false);
    refused(&r, "a draw that is not time-locked");
    // The plain C11 section, claimed time-locked by the chain's answer.
    let mut r = sealed_summary();
    r["sealed_reveals"]["chain"]["draw_time_locked"]["answer"] = json!(true);
    refused(&r, "posts without their time-lock read");
    let mut r = s.clone();
    let ct = h(r["sealed_reveals"]["posts"][0]["ciphertext"]
        .as_str()
        .expect("ct"));
    r["sealed_reveals"]["posts"][0]["ciphertext"] = json!(hx(&ct[..128]));
    refused(&r, "a post without room for a time-lock");
    // ROADMAP C52: another far round than the page derives from the template,
    // or none at all.
    let far = s["sealed_reveals"]["time_lock"]["far_round"]
        .as_u64()
        .expect("far");
    assert_eq!(out["sealed_reveals"]["far_round"], far);
    let mut r = s.clone();
    r["sealed_reveals"]["time_lock"]["far_round"] = json!(far + 1);
    refused(&r, "another far round");
    let mut r = s.clone();
    r["sealed_reveals"]["time_lock"]
        .as_object_mut()
        .expect("time lock")
        .remove("far_round");
    refused(&r, "no far round");
    // Review C52-R2-3: the chain's recorded slot holds the same 272 bytes, so
    // only the length rule can refuse it (before, the post differed from the
    // chain's and that alone refused it).
    let mut r = s.clone();
    let p = &r["sealed_reveals"]["posts"][0];
    let short = PostedSealed {
        ephemeral: h(p["ephemeral"].as_str().expect("e"))
            .try_into()
            .expect("64"),
        key_proof: h(p["key_proof"].as_str().expect("kp"))
            .try_into()
            .expect("kp"),
        posted_at: p["posted_at"].as_u64().expect("at"),
        ciphertext: ct[..272].to_vec(),
    };
    let (rv, kp) = posted_returns(Some(&short));
    r["sealed_reveals"]["posts"][0]["ciphertext"] = json!(hx(&ct[..272]));
    r["sealed_reveals"]["chain"]["posts"][0]["sealed_reveal_of"] = json!(hx(&rv));
    r["sealed_reveals"]["chain"]["posts"][0]["sealed_key_proof_of"] = json!(hx(&kp));
    refused(&r, "a post without room for the far copy");
    let out = check(&r);
    assert!(
        out["problems"]
            .as_array()
            .expect("problems")
            .iter()
            .any(|p| p
                .as_str()
                .unwrap_or_default()
                .contains("without a time-lock and far copy")),
        "{out}"
    );
}

/// Review C52-R2-3, end to end on the page: a drawn observer's post whose body
/// is its valid reveal but whose far copy is not its body key's is dismissed by
/// the coordinator (with its opening), and the page's `check` takes that
/// dismissal (it is never called false); the honest post "dismissed" with its
/// true opening is called false. (The observer is still listed as revealed in
/// this value round, so the page also reports that; only the far rule is
/// asserted here.)
#[test]
fn c52_r2_3_the_page_takes_a_far_copy_dismissal_end_to_end() {
    use onyx_coordinator::timelock::{
        far_copy, open_timelocked, seal_parts, wrap_key, QuicknetTimeLock, TimeLock,
        TimedDismissal, TimedOpened,
    };
    let s = timelocked_summary();
    assert_eq!(check(&s)["ok"], Value::Bool(true));
    let sp = spec(&s);
    let rid = h32(s["round_id"].as_str().expect("round id"));
    let attempt = u32::try_from(s["draw"]["attempt"].as_u64().expect("attempt")).expect("u32");
    let rc = s["config"]["reveal_close"].as_u64().expect("rc");
    let rounds = rounds_of(&s, rc);
    let rho = rounds.lock();
    let sig = sig_of(rho);
    let q = QuicknetTimeLock::pinned().expect("pinned");
    let key = public_point(&REVEAL).expect("key");
    let p0 = s["sealed_reveals"]["posts"][0].clone();
    let me = addr(p0["observer"].as_str().expect("observer"));
    let ct = h(p0["ciphertext"].as_str().expect("ct"));
    let honest = PostedSealed {
        ephemeral: h(p0["ephemeral"].as_str().expect("e"))
            .try_into()
            .expect("64"),
        key_proof: h(p0["key_proof"].as_str().expect("kp"))
            .try_into()
            .expect("kp"),
        posted_at: p0["posted_at"].as_u64().expect("at"),
        ciphertext: ct.clone(),
    };
    let commit = h32(s["reveals"][0]["commit"].as_str().expect("commit"));
    let ctx = RevealContext {
        round_id: rid,
        attempt,
        spec: &sp,
        commit: Some(commit),
    };
    // The same body under a far copy of another key (observer 0's secrets).
    let ls = lock_secrets(0);
    let lock: [u8; 128] = ct[..128].try_into().expect("tl");
    let bad_far = far_copy(&q, rounds.far(), &[0x77; 16]).expect("far");
    let shared = onyx_coordinator::sealed::shared_point(&ls.ephemeral, &key).expect("S");
    let bk = |far: &[u8; 128]| {
        onyx_coordinator::timelock::body_key(
            &ls.kappa,
            &honest.ephemeral,
            &lock,
            far,
            rounds,
            &onyx_tlock::QUICKNET_CHAIN_HASH,
            &rid,
            attempt,
            &me,
        )
    };
    let plain =
        onyx_coordinator::sealed::stream_xor(&bk(ct[128..256].try_into().expect("tf")), &ct[272..]);
    assert!(valid_reveal(&plain, &me, &ctx).is_some(), "the honest body");
    let wrap = wrap_key(
        &shared,
        &ls.tau,
        &honest.ephemeral,
        &lock,
        &bad_far,
        rounds,
        &onyx_tlock::QUICKNET_CHAIN_HASH,
        &rid,
        attempt,
        &me,
    );
    let mut w = [0u8; 16];
    for (i, x) in w.iter_mut().enumerate() {
        *x = wrap[i] ^ ls.kappa[i];
    }
    let bad = seal_parts(
        &key_of(&me),
        &me,
        &ls.ephemeral,
        &honest.ephemeral,
        &rid,
        attempt,
        &[
            &lock,
            &bad_far,
            &w,
            &onyx_coordinator::sealed::stream_xor(&bk(&bad_far), &plain),
        ],
    )
    .expect("seal");
    let posted = PostedSealed {
        ciphertext: bad.ciphertext.clone(),
        key_proof: bad.key_proof,
        ..honest.clone()
    };
    let Ok(TimedOpened::Dismissed(d)) =
        open_timelocked(&q, &REVEAL, Some(&sig), &me, &posted, &ctx, rounds)
    else {
        panic!("the coordinator dismisses a bad far copy");
    };
    let fake = TimedDismissal {
        dismissal: dismiss(
            &REVEAL,
            &onyx_coordinator::sealed::SealedPost::from_posted(&me, &honest),
            &rid,
            attempt,
        )
        .expect("dleq"),
        opening: q.open(rho, &sig, &lock),
    };
    let with = |p: &PostedSealed, d: &TimedDismissal| {
        let mut r = s.clone();
        let (rv, kp) = posted_returns(Some(p));
        r["sealed_reveals"]["posts"][0]["ciphertext"] = json!(hx(&p.ciphertext));
        r["sealed_reveals"]["posts"][0]["key_proof"] = json!(hx(&p.key_proof));
        r["sealed_reveals"]["chain"]["posts"][0]["sealed_reveal_of"] = json!(hx(&rv));
        r["sealed_reveals"]["chain"]["posts"][0]["sealed_key_proof_of"] = json!(hx(&kp));
        r["sealed_reveals"]["dismissals"] = json!([{
            "observer": hx(&me),
            "shared": hx(&d.dismissal.shared),
            "proof": hx(&d.dismissal.proof),
        }]);
        let o = d.opening.expect("the near time-lock opens");
        r["sealed_reveals"]["time_lock"]["openings"] = json!([{
            "observer": hx(&me),
            "sigma": hx(&o.sigma),
            "tau": hx(&o.secret),
        }]);
        let out = check(&r);
        let problems: Vec<String> = out["problems"]
            .as_array()
            .expect("problems")
            .iter()
            .map(|p| p.as_str().unwrap_or_default().to_string())
            .collect();
        assert!(
            !problems.iter().any(|p| p.contains("does not check")
                || p.contains("does not re-encrypt")
                || p.contains("not the chain")),
            "the section is otherwise well formed: {problems:?}"
        );
        problems.iter().any(|p| p.contains("is false"))
    };
    assert!(
        !with(&posted, &d),
        "a far-copy dismissal of a valid body stands on the page"
    );
    assert!(
        with(&honest, &fake),
        "the honest post dismissed with its true opening is false"
    );
}

/// Review C51-R2-1: a revealed post of a time-locked draw whose time-lock does
/// not open under the lock round (junk, or an honest body with its time-lock
/// zeroed) is refused by the page even when the chain's recorded slot holds
/// those very bytes (the registry's length rule cannot tell).
#[test]
fn c51_r2_1_a_revealed_post_whose_time_lock_does_not_open_is_refused() {
    let s = timelocked_summary();
    assert_eq!(check(&s)["ok"], Value::Bool(true));
    let ct = h(s["sealed_reveals"]["posts"][0]["ciphertext"]
        .as_str()
        .expect("ct"));
    let mut zeroed = ct.clone();
    zeroed[..128].fill(0);
    for bad in [vec![9u8; ct.len()], zeroed] {
        let mut r = s.clone();
        let p = &r["sealed_reveals"]["posts"][0];
        let posted = PostedSealed {
            ephemeral: h(p["ephemeral"].as_str().expect("e"))
                .try_into()
                .expect("33"),
            key_proof: h(p["key_proof"].as_str().expect("kp"))
                .try_into()
                .expect("kp"),
            posted_at: p["posted_at"].as_u64().expect("at"),
            ciphertext: bad.clone(),
        };
        let (rv, kp) = posted_returns(Some(&posted));
        r["sealed_reveals"]["posts"][0]["ciphertext"] = json!(hx(&bad));
        r["sealed_reveals"]["chain"]["posts"][0]["sealed_reveal_of"] = json!(hx(&rv));
        r["sealed_reveals"]["chain"]["posts"][0]["sealed_key_proof_of"] = json!(hx(&kp));
        let out = check(&r);
        assert_eq!(out["ok"], Value::Bool(false), "{out}");
        assert!(
            out["problems"]
                .as_array()
                .expect("problems")
                .iter()
                .any(|p| p
                    .as_str()
                    .unwrap_or_default()
                    .contains("does not open under the lock round")),
            "{out}"
        );
    }
}

// ------------------------------------------------------------ ROADMAP C52

/// The page's far-date mirrors are the coordinator's: the far round from the
/// pinned template (the record date's last observable day) for every reveal
/// close a config can take, the refusal past it, and the far-copy rule of a
/// dismissal: a post whose far copy is not its body key's is dismissed even
/// with a valid body (the page agrees: no false dismissal), while an honest
/// post "dismissed" with its true opening is a false dismissal on the page too.
#[test]
fn c52_the_page_far_date_mirrors_match_the_coordinator() {
    use onyx_coordinator::timelock::{
        check_timelocked_dismissal, far_copy, open_timelocked, seal_parts, seal_timelocked,
        wrap_key, BeaconChain, LockRounds, QuicknetTimeLock, TimeLock, TimedDismissal, TimedOpened,
        UNSEAL_MARGIN_SECS,
    };
    use onyx_web_verify::sealed::{check_timelocked, far_round, far_sigma};
    let r: Value = serde_json::from_str(SUMMARY).expect("json");
    let sp = spec(&r);
    let t = onyx_source_fiscaldata::template();
    let rc = r["config"]["reveal_close"].as_u64().expect("rc");
    let cc = r["config"]["commit_close"].as_u64().expect("cc");
    let q = QuicknetTimeLock::pinned().expect("pinned");
    for close in [rc, rc + 1, rc + 86_400, sp.window_end + 600 + 86_400] {
        assert_eq!(
            far_round(&t, &sp, close),
            Some(
                LockRounds::of_round(&BeaconChain::quicknet(), &t, &sp, close)
                    .expect("rounds")
                    .far()
            ),
            "close {close}"
        );
    }
    // A reveal close within a day of the fact's unseal date: both refuse.
    let unseal = onyx_coordinator::timelock::unseal_time(&t, &sp).expect("unseal");
    assert_eq!(far_round(&t, &sp, unseal - UNSEAL_MARGIN_SECS + 1), None);
    assert!(LockRounds::of_round(
        &BeaconChain::quicknet(),
        &t,
        &sp,
        unseal - UNSEAL_MARGIN_SECS + 1
    )
    .is_err());
    // Review C52-R2-2: the horizon's edges agree. A far date exactly
    // `MAX_UNSEAL_DELAY_SECS` after the reveal close is taken by both, a second
    // more is refused by both; a far date exactly a day after it is taken.
    let max = onyx_coordinator::timelock::MAX_UNSEAL_DELAY_SECS;
    for (close, ok) in [
        (unseal - max, true),
        (unseal - max - 1, false),
        (unseal - UNSEAL_MARGIN_SECS, true),
    ] {
        let native = LockRounds::of_round(&BeaconChain::quicknet(), &t, &sp, close);
        assert_eq!(native.is_ok(), ok, "native, close {close}");
        assert_eq!(
            far_round(&t, &sp, close),
            native.ok().map(|r| r.far()),
            "page, close {close}"
        );
    }
    // Review C52-R2-1: a template whose rights do not allow free
    // redistribution has no far round on the page either (the coordinator and
    // the Rust client refuse it: `NotRedistributable`).
    let mut restricted = t.clone();
    restricted.rights.redistribution = onyx_core::Redistribution::Restricted;
    let mut rsp = sp.clone();
    rsp.rights = restricted.rights.clone();
    assert!(restricted.matches(&rsp).is_ok());
    assert_eq!(
        LockRounds::of_round(&BeaconChain::quicknet(), &restricted, &rsp, rc),
        Err(onyx_coordinator::timelock::TimeLockError::NotRedistributable)
    );
    assert_eq!(far_round(&restricted, &rsp, rc), None);
    let kappa = [0x21u8; 16];
    assert_eq!(
        far_sigma(&kappa, 9),
        onyx_coordinator::timelock::far_sigma(&kappa, 9)
    );
    // An honest post, and the same post with a far copy of another key.
    let b = onyx_tlock::BeaconKey::quicknet().expect("key");
    let rounds = rounds_of(&r, rc);
    let rho = rounds.lock();
    let sig = sig_of(rho);
    let key = public_point(&REVEAL).expect("key");
    let rid = keccak256(b"c52 page round");
    let obs_key = keccak256(b"c52 observer").0;
    let me = onyx_labels::attest::secret_address(&obs_key).expect("key");
    let o = Observation {
        answer: Answer::Number(5),
        observed_at: sp.window_start + 1,
        page_hash: keccak256(b"page"),
    };
    let salt = keccak256(b"salt");
    let commit = onyx_core::observation_commit(&rid, &me, &o, &salt);
    let ctx = RevealContext {
        round_id: rid,
        attempt: 0,
        spec: &sp,
        commit: Some(commit),
    };
    let s = lock_secrets(3);
    let honest =
        seal_timelocked(&q, &obs_key, &s, &key, &rid, 0, rounds, cc, &sp, &o, &salt).expect("seal");
    let (lock, rest) = honest.ciphertext.split_at(128);
    let body = &rest[144..];
    let bad_far = far_copy(&q, rounds.far(), &[0x77; 16]).expect("far");
    let lock_arr: [u8; 128] = lock.try_into().expect("tl");
    let shared = onyx_coordinator::sealed::shared_point(&s.ephemeral, &key).expect("S");
    let wrap = wrap_key(
        &shared,
        &s.tau,
        &honest.ephemeral,
        &lock_arr,
        &bad_far,
        rounds,
        &onyx_tlock::QUICKNET_CHAIN_HASH,
        &rid,
        0,
        &me,
    );
    let mut w = [0u8; 16];
    for (i, x) in w.iter_mut().enumerate() {
        *x = wrap[i] ^ s.kappa[i];
    }
    // The body stays under the honest key's context bytes, which name the
    // honest far copy: re-encrypt it under this post's context instead.
    let k_bad = onyx_coordinator::timelock::body_key(
        &s.kappa,
        &honest.ephemeral,
        &lock_arr,
        &bad_far,
        rounds,
        &onyx_tlock::QUICKNET_CHAIN_HASH,
        &rid,
        0,
        &me,
    );
    let k_honest = onyx_coordinator::timelock::body_key(
        &s.kappa,
        &honest.ephemeral,
        &lock_arr,
        rest[..128].try_into().expect("tf"),
        rounds,
        &onyx_tlock::QUICKNET_CHAIN_HASH,
        &rid,
        0,
        &me,
    );
    let plain = onyx_coordinator::sealed::stream_xor(&k_honest, body);
    let bad = seal_parts(
        &obs_key,
        &me,
        &s.ephemeral,
        &honest.ephemeral,
        &rid,
        0,
        &[
            lock,
            &bad_far,
            &w,
            &onyx_coordinator::sealed::stream_xor(&k_bad, &plain),
        ],
    )
    .expect("seal");
    let posted_of = |ct: &[u8]| PostedSealed {
        ephemeral: honest.ephemeral,
        key_proof: honest.key_proof,
        posted_at: cc + 1,
        ciphertext: ct.to_vec(),
    };
    let pb = posted_of(&bad.ciphertext);
    let Ok(TimedOpened::Dismissed(d)) =
        open_timelocked(&q, &REVEAL, Some(&sig), &me, &pb, &ctx, rounds)
    else {
        panic!("a bad far copy is dismissed");
    };
    let want = check_timelocked_dismissal(&q, &key, &me, &pb, &ctx, rounds, Some(&sig), &d)
        .expect("the coordinator accepts the dismissal");
    let opening = d.opening.map(|x| (x.sigma, x.secret));
    let (page, may) = check_timelocked(
        &b,
        rho,
        rounds.far(),
        &sig,
        opening,
        &d.dismissal.shared,
        &honest.ephemeral,
        &rid,
        0,
        &me,
        &bad.ciphertext,
    )
    .expect("the page's check");
    assert_eq!(page, want);
    assert!(!may, "the far copy is not the body key's");
    assert!(
        valid_reveal(&page, &me, &ctx).is_some(),
        "its body is a valid reveal, dismissed for its far copy"
    );
    // The honest post "dismissed" with its true opening: false on the page.
    let ph = posted_of(&honest.ciphertext);
    let fake = TimedDismissal {
        dismissal: dismiss(
            &REVEAL,
            &onyx_coordinator::sealed::SealedPost::from_posted(&me, &ph),
            &rid,
            0,
        )
        .expect("dleq"),
        opening: q.open(rho, &sig, &lock_arr),
    };
    let (plain_h, may_h) = check_timelocked(
        &b,
        rho,
        rounds.far(),
        &sig,
        fake.opening.map(|x| (x.sigma, x.secret)),
        &fake.dismissal.shared,
        &honest.ephemeral,
        &rid,
        0,
        &me,
        &honest.ciphertext,
    )
    .expect("checks");
    assert!(may_h && valid_reveal(&plain_h, &me, &ctx).is_some());
    assert!(
        check_timelocked_dismissal(&q, &key, &me, &ph, &ctx, rounds, Some(&sig), &fake).is_err()
    );
    // A far round shifted by one: the page's far-copy check fails for the
    // honest post too, so only the template's far round is ever used.
    let (_, may_shift) = check_timelocked(
        &b,
        rho,
        rounds.far() + 1,
        &sig,
        fake.opening.map(|x| (x.sigma, x.secret)),
        &fake.dismissal.shared,
        &honest.ephemeral,
        &rid,
        0,
        &me,
        &honest.ciphertext,
    )
    .expect("checks");
    assert!(!may_shift);
}
