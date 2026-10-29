//! Level-3 key life cycle across several keys, sources and facts (ADR-0028
//! amendments 3-4, reviews L3R3-F5 and L3R4-F3). Three keys sign one shared
//! fact, backdated facts and sequence equivocations; relayers replay; a
//! second source tries to register a signer already taken; a reviewer
//! retires, compromises (from the start or from now), reinstates and
//! resolves; time advances past the retirement grace and past the pruning
//! window. Review L3R5-F3 added the events the earlier set could not reach:
//! reports dated ahead of now (within the clock skew), retirements with a
//! point in the past, compromises from an arbitrary time, sequence
//! equivocations on the shared fact, and a backdated fact shared between
//! keys.
//!
//! The reference model below is independent of the store: it keeps its own
//! life cycles, suspensions, fact book, sequence and payload books, accepted
//! set, re-review list and pruning, written from the spec (R-SIG-1..7), and
//! never reads the store's state. After every event the store must give the
//! model's exact result and hold the model's exact state. That implies the
//! three invariants:
//!
//! 1. A report is accepted only if its key's life cycle permits it and the
//!    key is not suspended.
//! 2. No suspension clears without a reviewer's reinstatement of that key,
//!    and no dispute ends without a reviewer's resolution.
//! 3. No retired or compromised key is ever re-enabled, and a signer is
//!    registered once, under one source.

use onyx_coordinator::signed::{
    signed_fact_key, FactStatus, KeyStatus, Reviewer, SignedError, SignedSources,
    MAX_REPORT_AGE_SECS, PRUNE_AFTER_SECS, RETIRED_GRACE_SECS,
};
use onyx_core::signed::REPORT_EVIDENCE;
use onyx_core::{
    keccak256, Claim, DataRights, EntityKind, EvidenceRef, Hash32, KeyProvenance, MetricId,
    MetricInfo, ProofLevel, Redistribution, SignedPayload, SignedReport, Snapshot, SourceId,
    SourceKey, SourceRef, StaticRegistry, Subject, Value, ValueKind, VerificationMethod,
};
use onyx_labels::attest::eip191_digest;
use secp256k1::{Message, Secp256k1, SecretKey};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

const NOW: u64 = 1_700_000_000;

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

/// Signatures already made, by (message, signer label).
type SigCache = HashMap<(Vec<u8>, String), [u8; 65]>;

thread_local! {
    static SIGS: RefCell<SigCache> = RefCell::new(HashMap::new());
}

/// EIP-191 signature, cached by (message, signer label): the exhaustive test
/// signs the same payloads many times.
fn sign(label: &str, msg: &[u8]) -> [u8; 65] {
    SIGS.with(|c| {
        *c.borrow_mut()
            .entry((msg.to_vec(), label.to_string()))
            .or_insert_with(|| {
                let digest = Message::from_digest(eip191_digest(msg).0);
                let (rid, bytes) = Secp256k1::new()
                    .sign_ecdsa_recoverable(&digest, &sk(label))
                    .serialize_compact();
                let mut out = [0u8; 65];
                out[..64].copy_from_slice(&bytes);
                out[64] = 27 + u8::try_from(rid.to_i32()).expect("recid");
                out
            })
    })
}

fn rights() -> DataRights {
    DataRights {
        license: "CC0-1.0".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId("example.signed".into()),
        attribution: None,
        terms: String::new(),
    }
}

fn reg() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.add_metric(
        "example.signed.reserve",
        MetricInfo {
            value_kind: ValueKind::UInt,
            needs_label_set: false,
        },
    )
    .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
    .expect("registry");
    r.add_source(rights()).expect("source");
    r.add_source(DataRights {
        source: SourceId("example.other".into()),
        ..rights()
    })
    .expect("second source");
    r
}

/// The same signer offered for the second source, under its own origin.
fn other_source_key(label: &str) -> SourceKey {
    SourceKey {
        source: SourceId("example.other".into()),
        signer: address(&sk(label)),
        provenance: KeyProvenance::WellKnown {
            origin: "other.example.org".into(),
            document_hash: keccak256(b"other keys document"),
            fetched_unix: NOW - 10_000,
        },
    }
}

fn source_key(label: &str) -> SourceKey {
    SourceKey {
        source: SourceId("example.signed".into()),
        signer: address(&sk(label)),
        provenance: KeyProvenance::WellKnown {
            origin: "data.example.org".into(),
            document_hash: keccak256(b"keys document"),
            fetched_unix: NOW - 10_000,
        },
    }
}

fn payload(key: Hash32, seq: u64, value: u128, observed: u64) -> SignedPayload {
    SignedPayload {
        source: SourceId("example.signed".into()),
        key,
        metric: MetricId("example.signed.reserve".into()),
        subject: Subject::Entity("entity:example".into()),
        value: Value::UInt(value),
        observed_unix: observed,
        sequence: seq,
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
        rights: rights(),
        evidence: vec![EvidenceRef {
            kind: REPORT_EVIDENCE.into(),
            content_hash: h,
        }],
        confidence: None,
    }
}

fn accept(
    s: &mut SignedSources,
    reg: &StaticRegistry,
    r: &SignedReport,
    now: u64,
) -> Result<ProofLevel, SignedError> {
    s.accept_claim(&claim_for(r), r, reg, now).map(|i| i.level)
}

const KEYS: [&str; 3] = ["K0", "K1", "K2"];
/// Length of the exhaustively enumerated sequences.
const DEPTH: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ev {
    /// Key `k` signs the shared fact with value `10 + v`.
    Sign(usize, u128),
    /// Key `k` signs a new fact observed six days ago.
    Backdate(usize),
    /// Key `k` signs a new payload under the sequence of its last report
    /// (a sequence equivocation; a fresh sequence if it has none).
    SeqEquivocate(usize),
    Retire(usize),
    /// Compromised from time 0 (its whole history withdrawn).
    Compromise(usize),
    /// Compromised from now (only what it stated since the last advance).
    CompromiseNow(usize),
    Reinstate(usize),
    /// The signer of key `k` offered again for the second source.
    RegisterOther(usize),
    /// A relayer resubmits the last report made.
    Replay,
    /// Resolve the shared fact keeping its first report, its last, or none.
    ResolveFirst,
    ResolveLast,
    ResolveClear,
    /// Time moves past every retirement grace.
    Advance,
    /// Time moves past the pruning window.
    AdvanceFar,
    /// Key `k` signs a fact observed 200 s ahead of now (within the clock
    /// skew); keys signing at the same time share it.
    SignAhead(usize),
    /// Key `k` retired with its point 100 s in the past.
    RetireBack(usize),
    /// Compromised from three days ago.
    CompromiseMid(usize),
    /// Key `k` signs the shared fact under the sequence of its last report
    /// (a sequence equivocation on a fact other keys state).
    SeqEquivocateShared(usize),
    /// Key `k` signs a fact backdated two days that every key can state
    /// (keys 0 and 2 agree, key 1 differs).
    BackdateShared(usize),
}

/// The full event set: [`events`] plus the L3R5-F3 additions.
fn events_full() -> Vec<Ev> {
    let mut e = events();
    for k in 0..KEYS.len() {
        e.push(Ev::SignAhead(k));
        e.push(Ev::RetireBack(k));
        e.push(Ev::CompromiseMid(k));
        e.push(Ev::SeqEquivocateShared(k));
        e.push(Ev::BackdateShared(k));
    }
    e
}

fn events() -> Vec<Ev> {
    let mut e = Vec::new();
    for k in 0..KEYS.len() {
        e.push(Ev::Sign(k, 0));
        e.push(Ev::Sign(k, 1));
        e.push(Ev::Backdate(k));
        e.push(Ev::SeqEquivocate(k));
        e.push(Ev::Retire(k));
        e.push(Ev::Compromise(k));
        e.push(Ev::CompromiseNow(k));
        e.push(Ev::Reinstate(k));
        e.push(Ev::RegisterOther(k));
    }
    e.extend([
        Ev::Replay,
        Ev::ResolveFirst,
        Ev::ResolveLast,
        Ev::ResolveClear,
        Ev::Advance,
        Ev::AdvanceFar,
    ]);
    e
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Life {
    Active,
    Retired(u64),
    Compromised(u64),
}

impl Life {
    fn rank(self) -> u8 {
        match self {
            Life::Active => 0,
            Life::Retired(_) => 1,
            Life::Compromised(_) => 2,
        }
    }
}

/// One report the model has seen.
#[derive(Clone, Debug)]
struct MRep {
    k: usize,
    value: u128,
    obs: u64,
    seq: u64,
    fact: Hash32,
    digest: Hash32,
    recorded_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MFact {
    Accepted(Hash32),
    Disputed(Vec<Hash32>),
}

/// The reference model: the spec's rules, written without the store.
#[derive(Default)]
struct Model {
    life: Vec<Life>,
    susp: Vec<bool>,
    /// Every report ever recorded (dropped ones too: their facts and
    /// accepted entries can outlive the books).
    reps: HashMap<Hash32, MRep>,
    /// Recorded and not yet pruned.
    present: BTreeSet<Hash32>,
    seqs: BTreeMap<(usize, u64), Hash32>,
    payloads: BTreeMap<Hash32, Hash32>,
    facts: BTreeMap<Hash32, MFact>,
    /// Accepted report -> accepted at.
    accepted: BTreeMap<Hash32, u64>,
    review: BTreeSet<Hash32>,
}

impl Model {
    fn permits(&self, k: usize, obs: u64, now: u64) -> Result<(), SignedError> {
        match self.life[k] {
            Life::Active => {}
            Life::Retired(at)
                if obs < at
                    && obs >= at.saturating_sub(RETIRED_GRACE_SECS)
                    && now < at + RETIRED_GRACE_SECS => {}
            Life::Retired(_) => return Err(SignedError::KeyRetired),
            Life::Compromised(_) => return Err(SignedError::KeyCompromised),
        }
        if self.susp[k] {
            return Err(SignedError::KeySuspended);
        }
        Ok(())
    }

    fn in_dispute(&self, h: &Hash32) -> bool {
        self.facts
            .values()
            .any(|f| matches!(f, MFact::Disputed(l) if l.contains(h)))
    }

    /// R-SIG-7 pruning: books forget reports observed before the window,
    /// except those still in a dispute or on the re-review list.
    fn prune(&mut self, now: u64) {
        let cutoff = now.saturating_sub(PRUNE_AFTER_SECS);
        let gone: Vec<Hash32> = self
            .present
            .iter()
            .filter(|h| {
                self.reps[h].obs < cutoff && !self.in_dispute(h) && !self.review.contains(h)
            })
            .copied()
            .collect();
        for h in gone {
            self.present.remove(&h);
            let r = &self.reps[&h];
            if self.seqs.get(&(r.k, r.seq)) == Some(&h) {
                self.seqs.remove(&(r.k, r.seq));
            }
            if self.payloads.get(&r.digest) == Some(&h) {
                self.payloads.remove(&r.digest);
            }
        }
    }

    fn record(&mut self, h: Hash32, r: MRep) {
        self.seqs.entry((r.k, r.seq)).or_insert(h);
        self.payloads.entry(r.digest).or_insert(h);
        self.present.insert(h);
        self.reps.entry(h).or_insert(r);
    }

    fn withdraw(&mut self, h: &Hash32) -> bool {
        if self.accepted.remove(h).is_some() {
            self.review.insert(*h);
            true
        } else {
            false
        }
    }

    /// The intake (R-SIG-2..6), as the spec orders it.
    fn accept(&mut self, h: Hash32, r: MRep, now: u64) -> Result<(), SignedError> {
        self.prune(now);
        self.permits(r.k, r.obs, now)?;
        if now.saturating_sub(r.obs) > MAX_REPORT_AGE_SECS {
            return Err(SignedError::Stale);
        }
        if self.payloads.contains_key(&r.digest) {
            return Err(SignedError::Replay);
        }
        if let Some(prev) = self.seqs.get(&(r.k, r.seq)).copied() {
            // R-SIG-6, last bullet: a different payload under a used
            // (key, sequence). The key is suspended; the report accepted
            // under that sequence is withdrawn and its fact disputed (with
            // both reports when they state the same fact). The second report
            // then meets the fact book on its own fact, short of acceptance.
            let (k, rfact, rvalue) = (r.k, r.fact, r.value);
            self.record(h, r);
            self.susp[k] = true;
            let pfact = self.reps[&prev].fact;
            let mut fact = rfact;
            let mut same_fact_handled = false;
            if self.facts.get(&pfact) == Some(&MFact::Accepted(prev)) {
                let mut list = vec![prev];
                if rfact == pfact {
                    list.push(h);
                    same_fact_handled = true;
                }
                self.facts.insert(pfact, MFact::Disputed(list));
                fact = pfact;
            }
            let mut withdrawn = vec![];
            if self.withdraw(&prev) {
                withdrawn.push(prev);
            }
            if !same_fact_handled {
                withdrawn.extend(self.meets(rfact, h, k, rvalue));
            }
            return Err(SignedError::Equivocation { fact, withdrawn });
        }
        let fact = r.fact;
        match self.facts.get(&fact).cloned() {
            None => {
                self.record(h, r);
                self.facts.insert(fact, MFact::Accepted(h));
                self.accepted.insert(h, now);
                Ok(())
            }
            Some(MFact::Accepted(prev)) => {
                if self.reps[&prev].value == r.value {
                    return Err(SignedError::AlreadyStated);
                }
                let k = r.k;
                self.record(h, r);
                self.susp[self.reps[&prev].k] = true;
                self.susp[k] = true;
                let withdrawn = if self.withdraw(&prev) {
                    vec![prev]
                } else {
                    vec![]
                };
                self.facts.insert(fact, MFact::Disputed(vec![prev, h]));
                Err(SignedError::Equivocation { fact, withdrawn })
            }
            Some(MFact::Disputed(list)) => {
                let own = list.iter().find(|x| self.reps[*x].k == r.k).copied();
                match own {
                    Some(o) if self.reps[&o].value == r.value => Err(SignedError::AlreadyStated),
                    Some(_) => {
                        let k = r.k;
                        self.record(h, r);
                        self.susp[k] = true;
                        Err(SignedError::Equivocation {
                            fact,
                            withdrawn: vec![],
                        })
                    }
                    None => {
                        let (k, value) = (r.k, r.value);
                        self.record(h, r);
                        let differs = list.iter().any(|x| self.reps[x].value != value);
                        if let Some(MFact::Disputed(l)) = self.facts.get_mut(&fact) {
                            l.push(h);
                        }
                        if differs {
                            self.susp[k] = true;
                            Err(SignedError::Equivocation {
                                fact,
                                withdrawn: vec![],
                            })
                        } else {
                            Err(SignedError::FactDisputed)
                        }
                    }
                }
            }
        }
    }

    /// R-SIG-6 for a report that is recorded but never accepted: a
    /// different value from another key's accepted report disputes the fact
    /// (both keys suspended, the accepted report withdrawn); a dispute it is
    /// not in yet takes it as evidence and suspends its key if its value
    /// differs; otherwise nothing changes.
    fn meets(&mut self, fact: Hash32, h: Hash32, k: usize, value: u128) -> Vec<Hash32> {
        match self.facts.get(&fact).cloned() {
            Some(MFact::Accepted(x)) => {
                if self.reps[&x].value == value {
                    return vec![];
                }
                self.susp[self.reps[&x].k] = true;
                self.susp[k] = true;
                let w = if self.withdraw(&x) { vec![x] } else { vec![] };
                self.facts.insert(fact, MFact::Disputed(vec![x, h]));
                w
            }
            Some(MFact::Disputed(list)) => {
                if list.contains(&h) || list.iter().any(|x| self.reps[x].k == k) {
                    return vec![];
                }
                let differs = list.iter().any(|x| self.reps[x].value != value);
                if let Some(MFact::Disputed(l)) = self.facts.get_mut(&fact) {
                    l.push(h);
                }
                if differs {
                    self.susp[k] = true;
                }
                vec![]
            }
            None => vec![],
        }
    }

    /// R-SIG-3: a retired key's reports count only when observed before
    /// `at`; those accepted while it was active but observed at or after
    /// `at` go to re-review and their facts are cleared.
    fn retire(&mut self, k: usize, at: u64) -> usize {
        self.life[k] = Life::Retired(at);
        let late: Vec<Hash32> = self
            .accepted
            .keys()
            .filter(|h| self.reps[*h].k == k && self.reps[*h].obs >= at)
            .copied()
            .collect();
        for h in &late {
            self.withdraw(h);
        }
        self.facts
            .retain(|_, f| !matches!(f, MFact::Accepted(h) if late.contains(h)));
        late.len()
    }

    /// R-SIG-6a: compromise withdraws what the key stated from `since` on.
    fn compromise(&mut self, k: usize, since: u64, now: u64) -> Vec<Hash32> {
        let since = match self.life[k] {
            Life::Compromised(s0) => s0.min(since),
            _ => since,
        };
        self.life[k] = Life::Compromised(since);
        let tainted: BTreeSet<Hash32> = self
            .reps
            .iter()
            .filter(|(h, r)| {
                r.k == k
                    && ((self.present.contains(*h) && (r.obs >= since || r.recorded_at >= since))
                        || self
                            .accepted
                            .get(*h)
                            .is_some_and(|at| r.obs >= since || *at >= since))
            })
            .map(|(h, _)| *h)
            .collect();
        let _ = now;
        let mut withdrawn: Vec<Hash32> = tainted
            .iter()
            .filter(|h| self.withdraw(h))
            .copied()
            .collect();
        withdrawn.sort();
        self.facts.retain(|_, f| match f {
            MFact::Accepted(h) => !tainted.contains(h),
            MFact::Disputed(l) => {
                l.retain(|h| !tainted.contains(h));
                true
            }
        });
        withdrawn
    }

    fn resolve(
        &mut self,
        fact: &Hash32,
        keep: Option<Hash32>,
        now: u64,
    ) -> Result<(), SignedError> {
        let Some(MFact::Disputed(list)) = self.facts.get(fact).cloned() else {
            return Err(SignedError::NotDisputed);
        };
        let Some(keep) = keep else {
            self.facts.remove(fact);
            return Ok(());
        };
        if !list.contains(&keep) {
            return Err(SignedError::BadResolution);
        }
        let r = &self.reps[&keep];
        self.permits(r.k, r.obs, now)
            .map_err(|_| SignedError::BadResolution)?;
        self.facts.insert(*fact, MFact::Accepted(keep));
        self.accepted.insert(keep, now);
        self.review.remove(&keep);
        Ok(())
    }
}

/// What the run saw, so the test can show every interesting branch ran.
#[derive(Default)]
struct Seen {
    accepted: usize,
    joiner_suspended: usize,
    joiner_recorded: usize,
    seq_equivocations: usize,
    replays_refused: usize,
    signer_taken: usize,
    partial_compromise: usize,
    pruned: usize,
    resolved_to_accepted: usize,
    reinstated: usize,
    retired_in_grace_accepted: usize,
    refused_suspended: usize,
    refused_life: usize,
    retire_withdrew: usize,
    seq_suspended_other: usize,
    ahead_accepted: usize,
    pairs: BTreeSet<(u8, bool, u8)>,
}

fn kind(ev: &Ev) -> u8 {
    match ev {
        Ev::Sign(..) => 0,
        Ev::Backdate(_) => 1,
        Ev::Retire(_) => 2,
        Ev::Compromise(_) | Ev::CompromiseNow(_) => 3,
        Ev::Reinstate(_) => 4,
        Ev::SeqEquivocate(_) => 5,
        Ev::RegisterOther(_) => 6,
        Ev::ResolveFirst | Ev::ResolveLast | Ev::ResolveClear => 7,
        Ev::Replay => 8,
        Ev::Advance | Ev::AdvanceFar => 9,
        Ev::SignAhead(_) => 0,
        Ev::RetireBack(_) => 2,
        Ev::CompromiseMid(_) => 3,
        Ev::SeqEquivocateShared(_) => 5,
        Ev::BackdateShared(_) => 1,
    }
}

fn actor(ev: &Ev) -> Option<usize> {
    match ev {
        Ev::Sign(k, _)
        | Ev::Backdate(k)
        | Ev::SeqEquivocate(k)
        | Ev::Retire(k)
        | Ev::Compromise(k)
        | Ev::CompromiseNow(k)
        | Ev::Reinstate(k)
        | Ev::RegisterOther(k)
        | Ev::SignAhead(k)
        | Ev::RetireBack(k)
        | Ev::CompromiseMid(k)
        | Ev::SeqEquivocateShared(k)
        | Ev::BackdateShared(k) => Some(*k),
        _ => None,
    }
}

/// Run one path on a fresh store and compare it with the model after each
/// event.
fn run(reg: &StaticRegistry, keys: &[SourceKey], path: &[Ev], seen: &mut Seen) {
    let (mut s, admin): (SignedSources, Reviewer) = SignedSources::with_reviewer("exhaustive");
    let ids: Vec<Hash32> = keys
        .iter()
        .map(|k| {
            s.register_key(&admin, k.clone(), reg, NOW, "setup")
                .expect("register")
        })
        .collect();
    let mut m = Model {
        life: vec![Life::Active; KEYS.len()],
        susp: vec![false; KEYS.len()],
        ..Model::default()
    };
    let shared_obs = NOW - 60;
    let shared = signed_fact_key(&payload(ids[0], 0, 0, shared_obs));
    let mut facts_seen: BTreeSet<Hash32> = BTreeSet::from([shared]);
    let mut made: Vec<(Hash32, SignedReport)> = Vec::new();
    let mut last_seq: Vec<Option<u64>> = vec![None; KEYS.len()];
    let mut t = NOW;
    for (i, ev) in path.iter().enumerate() {
        let step = i as u64 + 1;
        let susp_before = m.susp.clone();
        let disputed_before: BTreeSet<Hash32> = m
            .facts
            .iter()
            .filter(|(_, f)| matches!(f, MFact::Disputed(_)))
            .map(|(f, _)| *f)
            .collect();
        let a = actor(ev);
        seen.pairs.insert((
            a.map_or(9, |k| m.life[k].rank()),
            a.is_some_and(|k| susp_before[k]),
            kind(ev),
        ));
        let present_before = m.present.len();
        // A report to submit, if the event makes or replays one.
        let submit: Option<SignedReport> = match *ev {
            Ev::Sign(k, v) => Some(report(KEYS[k], payload(ids[k], step, 10 + v, shared_obs))),
            Ev::Backdate(k) => Some(report(
                KEYS[k],
                payload(ids[k], step, 99, t - 6 * 86_400 + step),
            )),
            Ev::SeqEquivocate(k) => Some(report(
                KEYS[k],
                payload(
                    ids[k],
                    last_seq[k].unwrap_or(1_000 + step),
                    77,
                    t - 5 * 86_400 + step,
                ),
            )),
            Ev::Replay => made.last().map(|(_, r)| r.clone()),
            Ev::SignAhead(k) => Some(report(
                KEYS[k],
                payload(ids[k], step, 30 + (k as u128 % 2), t + 200),
            )),
            Ev::SeqEquivocateShared(k) => Some(report(
                KEYS[k],
                payload(ids[k], last_seq[k].unwrap_or(2_000 + step), 55, shared_obs),
            )),
            Ev::BackdateShared(k) => Some(report(
                KEYS[k],
                payload(ids[k], step, 20 + (k as u128 % 2), NOW - 2 * 86_400),
            )),
            _ => None,
        };
        if let Some(r) = submit {
            let k = ids.iter().position(|id| *id == r.payload.key).expect("key");
            let h = r.hash();
            let fact = signed_fact_key(&r.payload);
            facts_seen.insert(fact);
            let Value::UInt(value) = r.payload.value else {
                unreachable!()
            };
            let mrep = MRep {
                k,
                value,
                obs: r.payload.observed_unix,
                seq: r.payload.sequence,
                fact,
                digest: r.payload.digest(),
                recorded_at: t,
            };
            let life_before = m.life[k];
            let want = m.accept(h, mrep, t);
            let got = accept(&mut s, reg, &r, t).map(|_| ());
            assert_eq!(got, want, "{path:?} step {step}");
            // Invariant 1, stated directly.
            if got.is_ok() {
                assert!(!susp_before[k], "{path:?} step {step}");
                seen.accepted += 1;
                if matches!(life_before, Life::Retired(_)) {
                    seen.retired_in_grace_accepted += 1;
                }
                if matches!(ev, Ev::SignAhead(_)) {
                    seen.ahead_accepted += 1;
                }
            }
            match &got {
                Err(SignedError::KeyRetired | SignedError::KeyCompromised) => {
                    seen.refused_life += 1
                }
                Err(SignedError::KeySuspended) => seen.refused_suspended += 1,
                Err(SignedError::Replay) => seen.replays_refused += 1,
                Err(SignedError::FactDisputed) => seen.joiner_recorded += 1,
                Err(SignedError::Equivocation { .. }) => {
                    let others = (0..KEYS.len()).any(|j| j != k && !susp_before[j] && m.susp[j]);
                    if matches!(ev, Ev::SeqEquivocateShared(_)) && others {
                        seen.seq_suspended_other += 1;
                    }
                    if matches!(ev, Ev::SeqEquivocate(_) | Ev::SeqEquivocateShared(_))
                        && last_seq[k].is_some()
                    {
                        seen.seq_equivocations += 1;
                    } else if !susp_before[k] && disputed_before.contains(&fact) {
                        seen.joiner_suspended += 1;
                    }
                }
                _ => {}
            }
            if !matches!(ev, Ev::Replay) {
                last_seq[k] = Some(r.payload.sequence);
                made.push((h, r));
            }
        }
        match *ev {
            Ev::Retire(k) | Ev::RetireBack(k) => {
                let at = if matches!(ev, Ev::Retire(_)) {
                    t
                } else {
                    t - 100
                };
                let got = s.retire_key(&admin, &ids[k], at, t, "rotate");
                if m.life[k] == Life::Active {
                    assert_eq!(got, Ok(()), "{path:?}");
                    if m.retire(k, at) > 0 {
                        seen.retire_withdrew += 1;
                    }
                } else {
                    assert_eq!(got, Err(SignedError::NotActive), "{path:?}");
                }
            }
            Ev::Compromise(k) | Ev::CompromiseNow(k) | Ev::CompromiseMid(k) => {
                let since = match ev {
                    Ev::Compromise(_) => 0,
                    Ev::CompromiseNow(_) => t,
                    _ => t - 3 * 86_400,
                };
                let mut got = s
                    .mark_compromised(&admin, &ids[k], since, t, "stolen")
                    .expect("compromise");
                got.sort();
                let want = m.compromise(k, since, t);
                assert_eq!(got, want, "{path:?} step {step}");
                if since > 0 && m.reps.values().any(|r| r.k == k) {
                    seen.partial_compromise += 1;
                }
            }
            Ev::Reinstate(k) => {
                let got = s.reinstate_key(&admin, &ids[k], t, "cleared");
                if m.susp[k] {
                    assert_eq!(got, Ok(()), "{path:?}");
                    m.susp[k] = false;
                    seen.reinstated += 1;
                } else {
                    assert_eq!(got, Err(SignedError::NotSuspended), "{path:?}");
                }
            }
            Ev::RegisterOther(k) => {
                let got = s.register_key(&admin, other_source_key(KEYS[k]), reg, t, "second");
                assert_eq!(got, Err(SignedError::SignerTaken), "{path:?} step {step}");
                seen.signer_taken += 1;
            }
            Ev::ResolveFirst | Ev::ResolveLast | Ev::ResolveClear => {
                let keep = match (m.facts.get(&shared), ev) {
                    (Some(MFact::Disputed(l)), Ev::ResolveFirst) => l.first().copied(),
                    (Some(MFact::Disputed(l)), Ev::ResolveLast) => l.last().copied(),
                    _ => None,
                };
                let got = s.resolve_fact(&admin, &shared, keep, t, "review");
                let want = m.resolve(&shared, keep, t);
                assert_eq!(got, want, "{path:?} step {step}");
                if got.is_ok() && keep.is_some() {
                    seen.resolved_to_accepted += 1;
                }
            }
            Ev::Advance => t += RETIRED_GRACE_SECS + 1,
            Ev::AdvanceFar => t += PRUNE_AFTER_SECS + 1,
            _ => {}
        }
        if m.present.len() < present_before {
            seen.pruned += 1;
        }
        // Invariant 2, from the model alone: a suspension clears only on a
        // reinstatement of that key, a dispute ends only on a resolution.
        for (j, was) in susp_before.iter().enumerate() {
            if *was && !m.susp[j] {
                assert_eq!(*ev, Ev::Reinstate(j), "{path:?}: suspension of {j} cleared");
            }
        }
        for f in &disputed_before {
            if !matches!(m.facts.get(f), Some(MFact::Disputed(_))) {
                assert!(
                    matches!(ev, Ev::ResolveFirst | Ev::ResolveLast | Ev::ResolveClear),
                    "{path:?}: dispute ended without a reviewer"
                );
            }
        }
        // The store holds exactly the model's state.
        for (j, id) in ids.iter().enumerate() {
            assert_eq!(s.is_suspended(id), m.susp[j], "{path:?}: suspension of {j}");
            let st = s.key_status(id).expect("key");
            let ok = match (m.life[j], st) {
                (Life::Active, KeyStatus::Active) => true,
                (Life::Retired(a), KeyStatus::Retired { at }) => a == at,
                (Life::Compromised(m0), KeyStatus::Compromised { since, .. }) => m0 == since,
                _ => false,
            };
            assert!(ok, "{path:?}: key {j} is {st:?}, model {:?}", m.life[j]);
        }
        for f in &facts_seen {
            let want = match m.facts.get(f) {
                None => None,
                Some(MFact::Accepted(h)) => Some(FactStatus::Accepted(*h)),
                Some(MFact::Disputed(l)) => Some(FactStatus::Disputed(l.clone())),
            };
            assert_eq!(
                s.fact_status(f).cloned(),
                want,
                "{path:?} step {step}: fact"
            );
        }
        for (h, _) in &made {
            assert_eq!(
                s.accepted(h).is_some(),
                m.accepted.contains_key(h),
                "{path:?} step {step}: accepted"
            );
        }
        assert_eq!(s.review_list(), &m.review, "{path:?} step {step}: review");
    }
}

/// Every sequence of four events from the 33 above, up to renaming the
/// keys (they are interchangeable: a path where key 1 acts before key 0 is
/// the renamed copy of one where key 0 acts first), so a key's first
/// appearance follows key order.
#[test]
fn every_four_event_sequence_matches_the_model() {
    let reg = reg();
    let keys: Vec<SourceKey> = KEYS.iter().map(|l| source_key(l)).collect();
    let ev = events();
    let mut seen = Seen::default();
    let mut n = 0usize;
    let mut path = Vec::new();
    fn walk(
        reg: &StaticRegistry,
        keys: &[SourceKey],
        ev: &[Ev],
        path: &mut Vec<Ev>,
        seen: &mut Seen,
        n: &mut usize,
    ) {
        if path.len() == DEPTH {
            run(reg, keys, path, seen);
            *n += 1;
            return;
        }
        let fresh = path.iter().filter_map(actor).max().map_or(0, |m| m + 1);
        for e in ev {
            if actor(e).is_some_and(|k| k > fresh) {
                continue;
            }
            path.push(*e);
            walk(reg, keys, ev, path, seen, n);
            path.pop();
        }
    }
    walk(&reg, &keys, &ev, &mut path, &mut seen, &mut n);
    assert!(n > 100_000, "{n} paths");
    assert!(seen.accepted > 0 && seen.refused_life > 0 && seen.refused_suspended > 0);
    assert!(seen.joiner_suspended > 0 && seen.seq_equivocations > 0);
    assert!(seen.replays_refused > 0 && seen.signer_taken > 0);
    assert!(seen.resolved_to_accepted > 0 && seen.reinstated > 0);
    assert!(seen.retired_in_grace_accepted > 0 && seen.partial_compromise > 0);
}

/// Every sequence of three events from the full set (the L3R5-F3 additions
/// included), up to renaming the keys.
#[test]
fn every_three_event_sequence_of_the_full_set_matches_the_model() {
    let reg = reg();
    let keys: Vec<SourceKey> = KEYS.iter().map(|l| source_key(l)).collect();
    let ev = events_full();
    let mut seen = Seen::default();
    let mut n = 0usize;
    for a in &ev {
        for b in &ev {
            for c in &ev {
                let path = [*a, *b, *c];
                // Keys appear in order of first use (renaming symmetry).
                let mut next = 0usize;
                let ordered = path.iter().filter_map(actor).all(|k| {
                    if k > next {
                        false
                    } else {
                        if k == next {
                            next += 1;
                        }
                        true
                    }
                });
                if !ordered {
                    continue;
                }
                run(&reg, &keys, &path, &mut seen);
                n += 1;
            }
        }
    }
    assert!(n > 20_000, "{n} paths");
    assert!(seen.retire_withdrew > 0, "no retirement withdrew a report");
    assert!(
        seen.seq_suspended_other > 0,
        "no sequence equivocation on another key's fact"
    );
    assert!(seen.ahead_accepted > 0, "no report dated ahead accepted");
}

/// Seeded random sequences of fourteen events: the long chains (a dispute,
/// joiners, a sequence equivocation, compromises of either side, replays,
/// reinstatements, resolutions, pruning) that four events cannot reach.
#[test]
fn random_fourteen_event_sequences_match_the_model() {
    let reg = reg();
    let keys: Vec<SourceKey> = KEYS.iter().map(|l| source_key(l)).collect();
    let ev = events_full();
    let mut seen = Seen::default();
    let mut x: u64 = 0x5eed_0ad5_1d3c_0004;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for _ in 0..6_000 {
        let path: Vec<Ev> = (0..14)
            .map(|_| ev[(next() % ev.len() as u64) as usize])
            .collect();
        run(&reg, &keys, &path, &mut seen);
    }
    assert!(seen.joiner_suspended > 0, "no contradicting joiner");
    assert!(seen.joiner_recorded > 0, "no agreeing joiner");
    assert!(seen.seq_equivocations > 0, "no sequence equivocation");
    assert!(seen.replays_refused > 0, "no replay");
    assert!(seen.signer_taken > 0, "no second-source registration");
    assert!(seen.partial_compromise > 0, "no compromise from now");
    assert!(seen.pruned > 0, "nothing pruned");
    assert!(seen.resolved_to_accepted > 0, "no reviewer resolution");
    assert!(seen.reinstated > 0, "no reinstatement");
    assert!(
        seen.retired_in_grace_accepted > 0,
        "no in-grace retired report"
    );
    assert!(seen.retire_withdrew > 0, "no retirement withdrew a report");
    assert!(
        seen.seq_suspended_other > 0,
        "no sequence equivocation on another key's fact"
    );
    assert!(seen.ahead_accepted > 0, "no report dated ahead accepted");
    for rank in 0..3u8 {
        for kind in [0u8, 1, 2, 3, 4, 5, 6] {
            assert!(
                seen.pairs.contains(&(rank, false, kind)),
                "missing life {rank} event {kind}"
            );
        }
    }
    assert!(seen.pairs.iter().any(|(_, susp, _)| *susp));
}
