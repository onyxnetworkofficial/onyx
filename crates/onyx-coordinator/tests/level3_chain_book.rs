//! Shared vectors and differential traces for the on-chain signed book
//! (`contracts/src/OnyxSignedBook.sol`, ADR-0028 amendment 6, THREATS L3-F7,
//! L3R-F8).
//!
//! - `fixtures/signed/vectors.json`: signed payloads of every `Subject` and
//!   `Value` variant with what the contract must compute from the Borsh bytes
//!   (payload digest, key id, fact key, value hash, report hash, observation
//!   time, sequence), plus malformed payloads it must refuse.
//! - `fixtures/signed/traces.json`: seeded random event sequences (three keys
//!   sign, equivocate, are retired, compromised, reinstated; relayers replay;
//!   the reviewer resolves; time advances) run through the off-chain store
//!   `SignedSources`. Every step records the store's result and, after it,
//!   the state of every fact of the trace and of every key. Foundry replays
//!   each trace against the contract (`OnyxSignedBookTraces.t.sol`) and
//!   requires the same results and states, so the contract's lazy
//!   withdrawals give the store's eager ones.
//!
//! The traces never advance far enough for the store to prune a sequence
//! (the chain never prunes; spec "Level 3 on chain"). Regenerate with
//! `ONYX_BLESS=1 cargo test -p onyx-coordinator --test level3_chain_book`
//! (only with an ADR change).

use onyx_coordinator::signed::{
    signed_fact_key, FactStatus, KeyStatus, Reviewer, SignedError, SignedSources,
};
use onyx_core::signed::REPORT_EVIDENCE;
use onyx_core::{
    keccak256, Claim, DataRights, EntityKind, EvidenceRef, Hash32, KeyProvenance, MetricId,
    MetricInfo, Redistribution, SignedPayload, SignedReport, Snapshot, SourceId, SourceKey,
    SourceRef, StaticRegistry, Subject, Value, ValueKind, VerificationMethod,
};
use onyx_labels::attest::eip191_digest;
use secp256k1::{Message, Secp256k1, SecretKey};
use serde_json::{json, Value as Json};
use std::collections::HashMap;

const NOW: u64 = 1_700_000_000;
const DAY: u64 = 86_400;
const SOURCE: &str = "example.signed";
const ORIGIN: &str = "data.example.org";
const METRIC: &str = "example.signed.reserve";
const KEYS: [&str; 3] = ["K0", "K1", "K2"];
const TRACES: usize = 160;
const STEPS: usize = 20;

fn sk(label: &str) -> SecretKey {
    SecretKey::from_slice(&keccak256(label.as_bytes()).0).expect("test key")
}

fn address(sk: &SecretKey) -> [u8; 20] {
    let pk = secp256k1::PublicKey::from_secret_key(&Secp256k1::new(), sk);
    let h = keccak256(&pk.serialize_uncompressed()[1..]);
    let mut a = [0u8; 20];
    a.copy_from_slice(&h.0[12..]);
    a
}

fn sign(label: &str, msg: &[u8]) -> [u8; 65] {
    let digest = Message::from_digest(eip191_digest(msg).0);
    let (rid, bytes) = Secp256k1::new()
        .sign_ecdsa_recoverable(&digest, &sk(label))
        .serialize_compact();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&bytes);
    out[64] = 27 + u8::try_from(rid.to_i32()).expect("recid");
    out
}

fn rights(id: &str) -> DataRights {
    DataRights {
        license: "CC0-1.0".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId(id.into()),
        attribution: None,
        terms: String::new(),
    }
}

fn reg() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.add_metric(
        METRIC,
        MetricInfo {
            value_kind: ValueKind::UInt,
            needs_label_set: false,
        },
    )
    .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
    .expect("registry");
    r.add_source(rights(SOURCE)).expect("source");
    r.add_source(rights("example.other"))
        .expect("second source");
    r
}

fn source_key(source: &str, origin: &str, label: &str) -> SourceKey {
    SourceKey {
        source: SourceId(source.into()),
        signer: address(&sk(label)),
        provenance: KeyProvenance::WellKnown {
            origin: origin.into(),
            document_hash: keccak256(b"keys document"),
            fetched_unix: NOW - 10_000,
        },
    }
}

fn report(label: &str, p: SignedPayload) -> SignedReport {
    let signature = sign(label, p.message().as_bytes());
    SignedReport {
        payload: p,
        signature,
    }
}

fn claim_for(r: &SignedReport) -> Claim {
    let h = r.hash();
    Claim {
        metric: r.payload.metric.clone(),
        subject: r.payload.subject.clone(),
        value: r.payload.value.clone(),
        source: SourceRef {
            source: r.payload.source.clone(),
            snapshot: Snapshot::Document {
                published_unix: r.payload.observed_unix,
                content_hash: h,
            },
        },
        method: VerificationMethod::SignedSource {
            key: r.payload.key,
            report: h,
        },
        label_set: None,
        rights: rights(SOURCE),
        evidence: vec![EvidenceRef {
            kind: REPORT_EVIDENCE.into(),
            content_hash: h,
        }],
        confidence: None,
    }
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn h32(h: &Hash32) -> String {
    hx(&h.0)
}

/// What the contract hashes as the value: keccak256(borsh(value)).
fn value_hash(v: &Value) -> Hash32 {
    keccak256(&borsh::to_vec(v).expect("borsh"))
}

fn write_or_check(path: &str, doc: &Json) {
    let text = serde_json::to_string_pretty(doc).expect("json") + "\n";
    let full = format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("ONYX_BLESS").is_some() {
        std::fs::create_dir_all(std::path::Path::new(&full).parent().expect("parent"))
            .expect("mkdir");
        std::fs::write(&full, &text).expect("write");
    }
    let on_disk = std::fs::read_to_string(&full)
        .unwrap_or_else(|_| panic!("{path} missing; run with ONYX_BLESS=1"));
    assert_eq!(on_disk, text, "{path} drifted from ADR-0028 amendment 6");
}

#[test]
fn signed_payload_vectors() {
    let key = source_key(SOURCE, ORIGIN, "V0");
    let id = key.id();
    let subjects = [
        Subject::Entity("entity:example".into()),
        Subject::Address("btc:bc1qexample".into()),
        Subject::Instrument("ticker:IBIT".into()),
        Subject::Aggregate("btc.mainnet".into()),
    ];
    let values = [
        Value::Int(-42),
        Value::UInt(u128::MAX),
        Value::Fixed {
            mantissa: -123_456,
            scale: 38,
        },
        Value::Bool(true),
        Value::Bytes(vec![]),
        Value::Bytes(vec![0xab; 1024]),
    ];
    let mut cases = Vec::new();
    for (i, subject) in subjects.iter().enumerate() {
        for (j, value) in values.iter().enumerate() {
            let p = SignedPayload {
                source: SourceId(SOURCE.into()),
                key: id,
                metric: MetricId(METRIC.into()),
                subject: subject.clone(),
                value: value.clone(),
                observed_unix: NOW + (i * 10 + j) as u64,
                sequence: (i * 100 + j) as u64,
            };
            let r = report("V0", p.clone());
            cases.push(json!({
                "payload": hx(&borsh::to_vec(&p).expect("borsh")),
                "signature": hx(&r.signature),
                "digest": h32(&p.digest()),
                "fact": h32(&signed_fact_key(&p)),
                "valueHash": h32(&value_hash(value)),
                "reportHash": h32(&r.hash()),
                "observed": p.observed_unix,
                "sequence": p.sequence,
            }));
        }
    }
    // Malformed payloads the contract must refuse (BadPayload); none decodes
    // as a SignedPayload in Rust either.
    let good = SignedPayload {
        source: SourceId(SOURCE.into()),
        key: id,
        metric: MetricId(METRIC.into()),
        subject: Subject::Entity("entity:example".into()),
        value: Value::UInt(7),
        observed_unix: NOW,
        sequence: 1,
    };
    let g = borsh::to_vec(&good).expect("borsh");
    let subject_at = 4 + SOURCE.len() + 32 + 4 + METRIC.len();
    let value_at = subject_at + 5 + "entity:example".len();
    let mut bad: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("truncated", g[..g.len() - 1].to_vec()),
        ("trailing byte", [g.clone(), vec![0]].concat()),
        ("source length past the end", {
            let mut b = g.clone();
            b[..4].copy_from_slice(&u32::MAX.to_le_bytes());
            b
        }),
    ];
    let mut t = g.clone();
    t[subject_at] = 4;
    bad.push(("subject tag 4", t));
    let mut t = g.clone();
    t[value_at] = 5;
    bad.push(("value tag 5", t));
    let fixed = |scale: u8| {
        borsh::to_vec(&SignedPayload {
            value: Value::Fixed { mantissa: 1, scale },
            ..good.clone()
        })
        .expect("borsh")
    };
    bad.push(("fixed scale 0", fixed(0)));
    bad.push(("fixed scale 39", fixed(39)));
    let mut t = borsh::to_vec(&SignedPayload {
        value: Value::Bool(true),
        ..good.clone()
    })
    .expect("borsh");
    t[value_at + 1] = 2;
    bad.push(("bool byte 2", t));
    bad.push((
        "bytes over 1024",
        borsh::to_vec(&SignedPayload {
            value: Value::Bytes(vec![1; 1025]),
            ..good.clone()
        })
        .expect("borsh"),
    ));
    bad.push((
        "zero key",
        borsh::to_vec(&SignedPayload {
            key: Hash32::ZERO,
            ..good.clone()
        })
        .expect("borsh"),
    ));
    bad.push((
        "zero observation time",
        borsh::to_vec(&SignedPayload {
            observed_unix: 0,
            ..good.clone()
        })
        .expect("borsh"),
    ));
    for (name, b) in &bad {
        // Rust refuses each too: by decoding or by `validate`.
        let refused = match borsh::from_slice::<SignedPayload>(b) {
            Err(_) => true,
            Ok(p) => p.validate().is_err() || borsh::to_vec(&p).expect("borsh") != *b,
        };
        assert!(refused, "{name} must be refused by Rust too");
    }
    let doc = json!({
        "source": SOURCE,
        "origin": ORIGIN,
        "signer": hx(&key.signer),
        "keyId": h32(&id),
        "cases": cases,
        "bad": bad.iter().map(|(n, b)| json!({"name": n, "payload": hx(b)})).collect::<Vec<_>>(),
    });
    write_or_check("fixtures/signed/vectors.json", &doc);
}

// ------------------------------------------------------------ traces

/// Result codes shared with `OnyxSignedBookTraces.t.sol`.
fn code(r: &Result<(), SignedError>) -> u64 {
    match r {
        Ok(()) => 0,
        Err(e) => match e {
            SignedError::Equivocation { .. } => 2,
            SignedError::FactDisputed => 3,
            SignedError::AlreadyStated => 10,
            SignedError::Replay => 11,
            SignedError::KeyRetired => 12,
            SignedError::KeyCompromised => 13,
            SignedError::KeySuspended => 14,
            SignedError::Stale => 15,
            SignedError::FromFuture => 16,
            SignedError::SequenceReserved => 17,
            SignedError::BadSignature => 18,
            SignedError::UnknownKey => 19,
            SignedError::KeySourceMismatch => 20,
            SignedError::NotActive => 21,
            SignedError::TimeInFuture => 22,
            SignedError::NotSuspended => 23,
            SignedError::NotDisputed => 24,
            SignedError::BadResolution => 25,
            SignedError::SignerTaken => 26,
            SignedError::KeyAlreadyRegistered => 27,
            SignedError::OriginMismatch => 28,
            SignedError::OriginTaken => 29,
            other => panic!("unexpected result in a trace: {other:?}"),
        },
    }
}

#[derive(Clone, Copy, Debug)]
enum Ev {
    /// Key `k` signs the shared fact with value `10 + v`.
    Sign(usize, u128),
    /// Key `k` signs its own new fact observed five days ago.
    Backdate(usize),
    /// Key `k` signs a fact backdated two days that every key can state
    /// (keys 0 and 2 agree, key 1 differs).
    BackdateShared(usize),
    /// Key `k` signs a fact observed 200 s ahead of now.
    SignAhead(usize),
    /// Key `k` signs a new fact under the sequence of its last report.
    SeqEquivocate(usize),
    /// Key `k` signs the shared fact under the sequence of its last report.
    SeqEquivocateShared(usize),
    Retire(usize),
    RetireBack(usize),
    Compromise(usize),
    CompromiseNow(usize),
    CompromiseMid(usize),
    Reinstate(usize),
    /// The signer of key `k` offered for the second source.
    RegisterOther(usize),
    /// A relayer posts the last report again.
    Replay,
    ResolveFirst,
    ResolveLast,
    ResolveClear,
    /// Two hours pass (past every grace and the dispute window).
    Advance,
    /// The reviewer reinstates the first suspended key with a report in the
    /// shared fact's dispute (key 0 when there is none).
    ReinstateDisputed,
    /// Key `k` signs a fact observed more than seven days ago (stale).
    SignStale(usize),
    /// Key `k` signs a fact observed 400 s ahead of now (from the future).
    SignFuture(usize),
    /// A stranger signs a payload naming key `k` (bad signature).
    Forge(usize),
    /// The reviewer registers a fresh signer for the second source (works
    /// once, then `KeyAlreadyRegistered`).
    RegisterFresh,
}

fn events() -> Vec<Ev> {
    let mut e = Vec::new();
    for k in 0..KEYS.len() {
        // Weighted (review C2-R1, finding 2): signing, reinstating and
        // resolving often enough that resolutions keeping a report succeed.
        e.extend([
            Ev::Sign(k, 0),
            Ev::Sign(k, 0),
            Ev::Sign(k, 1),
            Ev::Sign(k, 1),
            Ev::Reinstate(k),
            Ev::Reinstate(k),
            Ev::SignStale(k),
            Ev::SignFuture(k),
            Ev::Forge(k),
            Ev::Backdate(k),
            Ev::BackdateShared(k),
            Ev::SignAhead(k),
            Ev::SeqEquivocate(k),
            Ev::SeqEquivocateShared(k),
            Ev::Retire(k),
            Ev::RetireBack(k),
            Ev::Compromise(k),
            Ev::CompromiseNow(k),
            Ev::CompromiseMid(k),
            Ev::Reinstate(k),
            Ev::RegisterOther(k),
        ]);
    }
    e.extend([
        Ev::Replay,
        Ev::ResolveFirst,
        Ev::ResolveFirst,
        Ev::ResolveFirst,
        Ev::ResolveLast,
        Ev::ResolveLast,
        Ev::ResolveLast,
        Ev::ResolveClear,
        Ev::Advance,
        Ev::Advance,
        Ev::RegisterFresh,
        Ev::ReinstateDisputed,
        Ev::ReinstateDisputed,
        Ev::ReinstateDisputed,
        Ev::ReinstateDisputed,
    ]);
    e
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// One step as Foundry replays it.
struct Step {
    kind: u64,
    key: u64,
    now: u64,
    arg: u64,
    keep: Hash32,
    payload: Vec<u8>,
    signature: Vec<u8>,
    outcome: u64,
}

struct Run {
    store: SignedSources,
    reviewer: Reviewer,
    reg: StaticRegistry,
    ids: Vec<Hash32>,
    now: u64,
    seq: Vec<u64>,
    last_seq: Vec<Option<u64>>,
    own_facts: u64,
    last: Option<(usize, SignedReport)>,
    digest_of: HashMap<Hash32, Hash32>,
    key_of: HashMap<Hash32, usize>,
    facts: Vec<Hash32>,
}

impl Run {
    fn new() -> Self {
        let (mut store, reviewer) = SignedSources::with_reviewer("reviewer");
        let reg = reg();
        let ids = KEYS
            .iter()
            .map(|l| {
                store
                    .register_key(&reviewer, source_key(SOURCE, ORIGIN, l), &reg, NOW, "setup")
                    .expect("register")
            })
            .collect();
        Run {
            store,
            reviewer,
            reg,
            ids,
            now: NOW,
            seq: vec![1; KEYS.len()],
            last_seq: vec![None; KEYS.len()],
            own_facts: 0,
            last: None,
            digest_of: HashMap::new(),
            key_of: HashMap::new(),
            facts: Vec::new(),
        }
    }

    fn payload(&self, k: usize, value: u128, observed: u64, sequence: u64) -> SignedPayload {
        SignedPayload {
            source: SourceId(SOURCE.into()),
            key: self.ids[k],
            metric: MetricId(METRIC.into()),
            subject: Subject::Entity("entity:example".into()),
            value: Value::UInt(value),
            observed_unix: observed,
            sequence,
        }
    }

    fn fresh_seq(&mut self, k: usize) -> u64 {
        let s = self.seq[k];
        self.seq[k] += 1;
        self.last_seq[k] = Some(s);
        s
    }

    fn reused_seq(&mut self, k: usize) -> u64 {
        match self.last_seq[k] {
            Some(s) => s,
            None => self.fresh_seq(k),
        }
    }

    fn post(&mut self, k: usize, p: SignedPayload) -> Step {
        let r = report(KEYS[k], p);
        self.post_report(k, r)
    }

    fn post_report(&mut self, k: usize, r: SignedReport) -> Step {
        let fact = signed_fact_key(&r.payload);
        if !self.facts.contains(&fact) {
            self.facts.push(fact);
        }
        self.digest_of.insert(r.hash(), r.payload.digest());
        if let Some(i) = self.ids.iter().position(|id| *id == r.payload.key) {
            self.key_of.insert(r.hash(), i);
        }
        let res = self
            .store
            .accept_claim(&claim_for(&r), &r, &self.reg, self.now)
            .map(|_| ());
        let outcome = match res {
            Ok(()) => 1,
            ref e => code(e),
        };
        let step = Step {
            kind: 0,
            key: k as u64,
            now: self.now,
            arg: 0,
            keep: Hash32::ZERO,
            payload: borsh::to_vec(&r.payload).expect("borsh"),
            signature: r.signature.to_vec(),
            outcome,
        };
        self.last = Some((k, r));
        step
    }

    fn admin(
        &self,
        kind: u64,
        k: usize,
        arg: u64,
        keep: Hash32,
        r: Result<(), SignedError>,
    ) -> Step {
        Step {
            kind,
            key: k as u64,
            now: self.now,
            arg,
            keep,
            payload: vec![],
            signature: vec![],
            outcome: code(&r),
        }
    }

    fn shared_fact(&self) -> Hash32 {
        signed_fact_key(&self.payload(0, 0, NOW, 0))
    }

    fn apply(&mut self, ev: Ev) -> Step {
        match ev {
            Ev::Sign(k, v) => {
                let s = self.fresh_seq(k);
                self.post(k, self.payload(k, 10 + v, NOW, s))
            }
            Ev::Backdate(k) => {
                let s = self.fresh_seq(k);
                self.own_facts += 1;
                let obs = self.now - 5 * DAY + self.own_facts;
                self.post(k, self.payload(k, 1, obs, s))
            }
            Ev::BackdateShared(k) => {
                let s = self.fresh_seq(k);
                let v = if k == 1 { 8 } else { 7 };
                self.post(k, self.payload(k, v, NOW - 2 * DAY, s))
            }
            Ev::SignAhead(k) => {
                let s = self.fresh_seq(k);
                self.post(k, self.payload(k, 5, self.now + 200, s))
            }
            Ev::SeqEquivocate(k) => {
                let s = self.reused_seq(k);
                self.own_facts += 1;
                let obs = self.now - 1_000 - self.own_facts;
                self.post(k, self.payload(k, 99, obs, s))
            }
            Ev::SeqEquivocateShared(k) => {
                let s = self.reused_seq(k);
                self.post(k, self.payload(k, 50 + k as u128, NOW, s))
            }
            Ev::Retire(k) => self.retire(k, self.now),
            Ev::RetireBack(k) => self.retire(k, self.now - 100),
            Ev::Compromise(k) => self.compromise(k, 0),
            Ev::CompromiseNow(k) => self.compromise(k, self.now),
            Ev::CompromiseMid(k) => self.compromise(k, self.now - 3 * DAY),
            Ev::Reinstate(k) => {
                let r = self
                    .store
                    .reinstate_key(&self.reviewer, &self.ids[k], self.now, "trace");
                self.admin(3, k, 0, Hash32::ZERO, r)
            }
            Ev::RegisterOther(k) => {
                let r = self
                    .store
                    .register_key(
                        &self.reviewer,
                        source_key("example.other", "other.example.org", KEYS[k]),
                        &self.reg,
                        self.now,
                        "trace",
                    )
                    .map(|_| ());
                self.admin(5, k, 0, Hash32::ZERO, r)
            }
            Ev::Replay => match self.last.clone() {
                Some((k, r)) => self.post(k, r.payload),
                None => {
                    let s = self.fresh_seq(0);
                    self.post(0, self.payload(0, 10, NOW, s))
                }
            },
            Ev::ResolveFirst | Ev::ResolveLast | Ev::ResolveClear => {
                let fact = self.shared_fact();
                // The first (or last) report whose key is not suspended,
                // else the first (or last): resolutions that keep a report
                // must happen often (review C2-R1, finding 2).
                let free = |h: &&Hash32| {
                    self.key_of
                        .get(*h)
                        .is_some_and(|k| !self.store.is_suspended(&self.ids[*k]))
                };
                let keep = match (ev, self.store.fact_status(&fact)) {
                    (Ev::ResolveFirst, Some(FactStatus::Disputed(l))) => {
                        l.iter().find(free).or(l.first()).copied()
                    }
                    (Ev::ResolveLast, Some(FactStatus::Disputed(l))) => {
                        l.iter().rev().find(free).or(l.last()).copied()
                    }
                    _ => None,
                };
                if !self.facts.contains(&fact) {
                    self.facts.push(fact);
                }
                let r = self
                    .store
                    .resolve_fact(&self.reviewer, &fact, keep, self.now, "trace");
                let keep_digest = keep.map_or(Hash32::ZERO, |h| self.digest_of[&h]);
                self.admin(4, 0, 0, keep_digest, r)
            }
            Ev::Advance => {
                self.now += 7_200;
                self.admin(6, 0, 0, Hash32::ZERO, Ok(()))
            }
            Ev::ReinstateDisputed => {
                let k = match self.store.fact_status(&self.shared_fact()) {
                    Some(FactStatus::Disputed(l)) => l
                        .iter()
                        .filter_map(|h| self.key_of.get(h).copied())
                        .find(|k| self.store.is_suspended(&self.ids[*k]))
                        .unwrap_or(0),
                    _ => 0,
                };
                let r = self
                    .store
                    .reinstate_key(&self.reviewer, &self.ids[k], self.now, "trace");
                self.admin(3, k, 0, Hash32::ZERO, r)
            }
            Ev::SignStale(k) => {
                let s = self.fresh_seq(k);
                let obs = self.now - 7 * DAY - 10;
                self.post(k, self.payload(k, 3, obs, s))
            }
            Ev::SignFuture(k) => {
                let s = self.fresh_seq(k);
                self.post(k, self.payload(k, 4, self.now + 400, s))
            }
            Ev::Forge(k) => {
                let s = self.fresh_seq(k);
                let p = self.payload(k, 666, NOW - 7, s);
                let r = report("stranger", p);
                self.post_report(k, r)
            }
            Ev::RegisterFresh => {
                let r = self
                    .store
                    .register_key(
                        &self.reviewer,
                        source_key("example.other", "other.example.org", "FRESH"),
                        &self.reg,
                        self.now,
                        "trace",
                    )
                    .map(|_| ());
                self.admin(7, 0, 0, Hash32::ZERO, r)
            }
        }
    }

    fn retire(&mut self, k: usize, at: u64) -> Step {
        let r = self
            .store
            .retire_key(&self.reviewer, &self.ids[k], at, self.now, "trace");
        self.admin(1, k, at, Hash32::ZERO, r)
    }

    fn compromise(&mut self, k: usize, since: u64) -> Step {
        let r = self
            .store
            .mark_compromised(&self.reviewer, &self.ids[k], since, self.now, "trace")
            .map(|_| ());
        self.admin(2, k, since, Hash32::ZERO, r)
    }

    /// Life code (1 active, 2 retired, 3 compromised) in bits 4k..4k+1 and
    /// the suspension in bit 4k+2, per key.
    fn keys_word(&self) -> u64 {
        let mut w = 0;
        for (k, id) in self.ids.iter().enumerate() {
            let st = self.store.key_state(id).expect("key");
            let life = match st.status {
                KeyStatus::Active => 1,
                KeyStatus::Retired { .. } => 2,
                KeyStatus::Compromised { .. } => 3,
            };
            let susp = u64::from(st.suspended_at.is_some());
            w |= (life | susp << 2) << (4 * k);
        }
        w
    }

    /// keccak256 over every fact of `facts`, in order: state (0 none,
    /// 1 accepted, 2 disputed) u8 ‖ accepted report hash (zero unless
    /// accepted) ‖ accepted at u64 BE ‖ settleable u8 ‖ dispute length u32 BE
    /// ‖ the dispute's report hashes. The Solidity side builds the same bytes
    /// from `factView` and `settleable`.
    fn facts_digest(&self, facts: &[Hash32]) -> Hash32 {
        let mut b = Vec::new();
        for f in facts {
            let (state, acc, at, list): (u8, Hash32, u64, Vec<Hash32>) =
                match self.store.fact_status(f) {
                    None => (0, Hash32::ZERO, 0, vec![]),
                    Some(FactStatus::Accepted(h)) => {
                        let at = self.store.accepted(h).expect("accepted").accepted_at;
                        (1, *h, at, vec![])
                    }
                    Some(FactStatus::Disputed(l)) => (2, Hash32::ZERO, 0, l.clone()),
                };
            b.push(state);
            b.extend_from_slice(&acc.0);
            b.extend_from_slice(&at.to_be_bytes());
            b.push(u8::from(self.store.settleable(f, self.now).is_some()));
            b.extend_from_slice(&u32::try_from(list.len()).expect("len").to_be_bytes());
            for h in list {
                b.extend_from_slice(&h.0);
            }
        }
        keccak256(&b)
    }
}

fn trace(seed: u64) -> Json {
    let evs = events();
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let picks: Vec<Ev> = (0..STEPS)
        .map(|_| evs[(rng.next() % evs.len() as u64) as usize])
        .collect();
    // First pass: the facts the trace touches (states are reported for all
    // of them after every step).
    let mut first = Run::new();
    for ev in &picks {
        first.apply(*ev);
    }
    let facts = first.facts.clone();
    // Second pass: the same events with the states.
    let mut run = Run::new();
    let mut steps = Vec::new();
    for ev in &picks {
        let s = run.apply(*ev);
        steps.push(json!({
            "kind": s.kind,
            "key": s.key,
            "now": s.now,
            "arg": s.arg,
            "keep": h32(&s.keep),
            "payload": hx(&s.payload),
            "signature": hx(&s.signature),
            "outcome": s.outcome,
            "keys": run.keys_word(),
            "facts": h32(&run.facts_digest(&facts)),
        }));
    }
    json!({
        "seed": seed,
        "events": format!("{picks:?}"),
        "facts": facts.iter().map(h32).collect::<Vec<_>>(),
        "steps": steps,
    })
}

#[test]
fn differential_traces_for_the_chain_book() {
    let traces: Vec<Json> = (1..=TRACES as u64).map(trace).collect();
    // The traces must reach every outcome the contract can give, or they
    // prove little.
    let mut seen = std::collections::BTreeSet::new();
    for t in &traces {
        for s in t["steps"].as_array().expect("steps") {
            seen.insert(s["outcome"].as_u64().expect("outcome"));
        }
    }
    for want in [
        0, 1, 2, 3, 10, 11, 12, 13, 14, 15, 16, 18, 21, 23, 24, 25, 26, 27,
    ] {
        assert!(seen.contains(&want), "no trace reaches outcome {want}");
    }
    // Review C2-R1 finding 2: enough of the rare paths that matter.
    let count = |f: &dyn Fn(&Json) -> bool| -> usize {
        traces
            .iter()
            .flat_map(|t| t["steps"].as_array().expect("steps").iter())
            .filter(|s| f(s))
            .count()
    };
    let zero = h32(&Hash32::ZERO);
    let kept = count(&|s| {
        s["kind"] == 4 && s["outcome"] == 0 && s["keep"].as_str() != Some(zero.as_str())
    });
    let joined = count(&|s| s["outcome"] == 3);
    assert!(kept >= 10, "only {kept} resolutions kept a report");
    assert!(
        joined >= 5,
        "only {joined} reports joined a dispute as evidence"
    );
    let keys: Vec<Json> = KEYS
        .iter()
        .map(|l| json!({ "label": l, "signer": hx(&address(&sk(l))) }))
        .collect();
    let shared = Run::new().shared_fact();
    let doc = json!({
        "source": SOURCE,
        "origin": ORIGIN,
        "sharedFact": h32(&shared),
        "otherSource": "example.other",
        "otherOrigin": "other.example.org",
        "provenance": h32(&keccak256(b"keys document")),
        "start": NOW,
        "count": traces.len(),
        "stepsPerTrace": STEPS,
        "keys": keys,
        "freshSigner": hx(&address(&sk("FRESH"))),
        "traces": traces,
    });
    write_or_check("fixtures/signed/traces.json", &doc);
}

/// L3R-F8 (and the L3R3-F6 / L3R4-F5 residual): the store's clock never
/// steps back. Before, a coordinator clock stepped back past the pruning
/// window let a pruned report be accepted again; now every action refuses a
/// `now` earlier than one already used, and a refusal consumes nothing.
#[test]
fn the_clock_never_steps_back() {
    let mut run = Run::new();
    let r = report("K0", run.payload(0, 10, NOW, 1));
    let accept = |run: &mut Run, r: &SignedReport, now: u64| {
        run.store
            .accept_claim(&claim_for(r), r, &run.reg, now)
            .map(|_| ())
    };
    accept(&mut run, &r, NOW).expect("accepted");
    // Eight days on, another report prunes the first one's sequence entry.
    let later = NOW + 8 * DAY;
    let r2 = report("K1", run.payload(1, 3, later, 1));
    accept(&mut run, &r2, later).expect("accepted");
    assert_eq!(run.store.clock(), later);
    // A step back to replay the pruned report (or anything) is refused.
    let back = NOW + DAY;
    assert_eq!(
        accept(&mut run, &r, back),
        Err(SignedError::ClockWentBack {
            now: back,
            clock: later
        })
    );
    let r3 = report("K2", run.payload(2, 4, back, 1));
    assert!(matches!(
        accept(&mut run, &r3, back),
        Err(SignedError::ClockWentBack { .. })
    ));
    let id = run.ids[0];
    assert!(matches!(
        run.store.retire_key(&run.reviewer, &id, back, back, "back"),
        Err(SignedError::ClockWentBack { .. })
    ));
    assert!(matches!(
        run.store
            .mark_compromised(&run.reviewer, &id, back, back, "back"),
        Err(SignedError::ClockWentBack { .. })
    ));
    // Pruning with an earlier time is a no-op, and nothing moved the clock.
    run.store.prune(back);
    assert_eq!(run.store.clock(), later);
    assert_eq!(run.store.key_status(&id), Some(KeyStatus::Active));
    // The same time again is fine (one block, several transactions).
    let r4 = report("K2", run.payload(2, 4, later - 5, 2));
    accept(&mut run, &r4, later).expect("same time accepted");
}

/// Review C2-R1 finding 4: a refused action does not move the clock, so a
/// wrong time in a refused claim (milliseconds for seconds, a bad RPC
/// answer) cannot wedge the store. A time ahead that the store does act on
/// still moves it (THREATS L3C-4: callers pass the chain's block time).
#[test]
fn a_refused_action_does_not_move_the_clock() {
    let mut run = Run::new();
    let ms = NOW * 1_000;
    let r = report("K0", run.payload(0, 10, NOW, 1));
    assert_eq!(
        run.store
            .accept_claim(&claim_for(&r), &r, &run.reg, ms)
            .map(|_| ()),
        Err(SignedError::Stale)
    );
    let id = run.ids[1];
    assert_eq!(
        run.store
            .reinstate_key(&run.reviewer, &id, ms, "nothing to reinstate"),
        Err(SignedError::NotSuspended)
    );
    assert_eq!(run.store.clock(), NOW, "registration time only");
    run.store
        .accept_claim(&claim_for(&r), &r, &run.reg, NOW + 5)
        .expect("the store still takes chain time");
    assert_eq!(run.store.clock(), NOW + 5);
}
