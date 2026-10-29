//! Serving checks (ADR-0034 Amendment 1): a metric whose rows a recipe rerun
//! cannot fully check (a reserve shard's base is in its params, and miner
//! and validators agree on any base) is served only after a check the
//! sector adapter supplies has cleared each finalized shard and each row.
//!
//! - A [`ServingGate`] judges every finalized shard of its metric at once
//!   (a reserve chain is judged as a chain), from what this server holds:
//!   the binding inputs, the source snapshot, the params (from the store, by
//!   the params hash the shard id commits to) and the leaves that hash to
//!   the root the chain finalized. It returns, per shard, the rows it clears
//!   (each with the basis of its cross-check) or why the shard is refused.
//! - [`refresh`] runs every gate over the catalog (after each indexer poll,
//!   and once at start) and stores the answers keyed by (round, shard) and
//!   the root judged.
//! - Which shards need a check is decided by the binding, not only by the
//!   catalog entry (review RESAPI-R1 F1): a shard needs a gate if it names
//!   one ([`ShardMeta::serving_check`]), if its metric has one, if a
//!   registered gate claims its binding ([`ServingGate::claims`]: schema id
//!   or recipe id), or if [`KNOWN_GATES`] lists its schema, recipe or metric
//!   name (review RESAPI-R2-1). The last is compiled into every server, so a
//!   server without that gate (plain `onyx-node`) still refuses the shard
//!   however the catalog names it. Two different gates for one shard refuse
//!   it.
//! - Requests: a shard that needs a gate is served only with an answer for
//!   its current root (fail closed): a missing gate is 403
//!   `serving_check_unavailable`, no answer yet 503 `serving_check_pending`,
//!   a refused shard 403 `serving_check_failed`, a row without a cross-check
//!   403 `not_cross_checked`. All of these come before any 402, so nothing is
//!   charged for a refusal.
//! - Liveness (review RESAPI-R1 F3): [`spawn_refresh`] repeats [`refresh`]
//!   on a timer, so a listed shard that finalizes later is judged without a
//!   restart. A root read that fails (a node error, not "not final") does
//!   not refuse the metric: the shard is judged at its last judged root, or,
//!   if it has none, the run keeps the previous answers of every shard whose
//!   root is unchanged and leaves the rest pending.
//! - The answer is the operator's check, not a proof: a client that does not
//!   trust the operator runs the same check on the same public inputs.

use crate::catalog::ShardMeta;
use crate::ApiState;
use onyx_coordinator::cas::{CasExt, Kind};
use onyx_core::{keccak256, Hash32, SourceRef};
use onyx_merkle::MerkleTree;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

/// One finalized shard as the server holds it, handed to a gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateShard {
    /// Round.
    pub round: u64,
    /// Shard id.
    pub shard: Hash32,
    /// The root the chain finalized (the leaves hash to it).
    pub root: Hash32,
    /// The pinned source and snapshot.
    pub source: SourceRef,
    /// Recipe id of the binding.
    pub recipe_id: Hash32,
    /// `LabelSetRef::hash` of the binding.
    pub label_set_hash: Hash32,
    /// Row schema id.
    pub schema_id: Hash32,
    /// The params bytes, when the store holds a blob with the params hash.
    pub params: Option<Vec<u8>>,
    /// Leaves in tree order.
    pub leaves: Vec<Vec<u8>>,
}

/// What a row may be served on: the basis of its entity's independent check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrossCheck {
    /// e.g. `published-por-file`, `issuer-wallet-list`, `second-node-rescan`.
    pub basis: String,
    /// What it was checked against.
    pub source: String,
}

/// One row's clearance: its key and its cross-check, or why it is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowClearance {
    /// The row key (the subject).
    pub key: Vec<u8>,
    /// The basis it is served on, or why not.
    pub cross_check: Result<CrossCheck, String>,
}

/// A gate's answer for one shard: every row's clearance by leaf index, or
/// why the shard is refused.
pub type Judgement = Result<Vec<RowClearance>, String>;

/// A sector's serving check for one metric.
pub trait ServingGate: Send + Sync {
    /// Name a shard's `serving_check` refers to.
    fn name(&self) -> &str;
    /// The metric whose shards it judges.
    fn metric(&self) -> &str;
    /// True if `meta`'s binding (schema id, recipe id) is one this gate
    /// judges, whatever metric or `servingCheck` the catalog gives it
    /// (review RESAPI-R1 F1: a gate keyed on the catalog's names alone fails
    /// open on a misnamed entry).
    fn claims(&self, meta: &ShardMeta) -> bool;
    /// Judge all finalized shards of the metric at once; one answer per
    /// input shard, in input order. Missing answers count as refusals.
    fn judge(&self, shards: &[GateShard]) -> Vec<Judgement>;
}

/// The answer stored for one shard.
#[derive(Clone, Debug)]
struct Entry {
    gate: String,
    root: Hash32,
    verdict: Result<Arc<Vec<RowClearance>>, String>,
}

/// One run of one gate, for `/v1/health`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateRun {
    /// Gate name.
    pub gate: String,
    /// Finalized shards judged.
    pub judged: usize,
    /// Of those, cleared.
    pub cleared: usize,
    /// Shards of the metric skipped: not final, rows not held here, or a
    /// failed root read with no earlier answer.
    pub skipped: usize,
    /// Root reads that failed (node errors) in this run.
    pub read_errors: usize,
    /// Unix time of the run.
    pub at: u64,
    /// The first refusal's reason, if any.
    pub first_refusal: Option<String>,
}

/// The server's gates and their latest answers.
#[derive(Default)]
pub struct Gates {
    by_name: BTreeMap<String, Arc<dyn ServingGate>>,
    by_metric: BTreeMap<String, String>,
    answers: RwLock<BTreeMap<(u64, Hash32), Entry>>,
    runs: RwLock<BTreeMap<String, GateRun>>,
}

impl Gates {
    /// Add `gate` (its name and metric must be new).
    pub fn add(&mut self, gate: Arc<dyn ServingGate>) -> Result<(), String> {
        let (n, m) = (gate.name().to_string(), gate.metric().to_string());
        if self.by_name.contains_key(&n) || self.by_metric.contains_key(&m) {
            return Err(format!("a gate {n} or one for {m} is already registered"));
        }
        self.by_metric.insert(m, n.clone());
        self.by_name.insert(n, gate);
        Ok(())
    }

    /// True if no gate is registered.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// The gate a shard needs, if any: every name that applies (the one it
    /// names, its metric's, any registered gate claiming its binding, and
    /// [`KNOWN_GATES`]) must be the same one, or the shard is refused.
    fn needed(&self, meta: &ShardMeta) -> Result<Option<String>, Refusal> {
        let mut names: BTreeSet<String> = BTreeSet::new();
        names.extend(meta.serving_check.iter().cloned());
        names.extend(self.by_metric.get(&meta.metric).cloned());
        names.extend(
            self.by_name
                .iter()
                .filter(|(_, g)| g.claims(meta))
                .map(|(n, _)| n.clone()),
        );
        names.extend(known_claim(meta).map(str::to_string));
        match names.len() {
            0 => Ok(None),
            1 => Ok(names.pop_first()),
            _ => Err(Refusal::Failed(format!(
                "the shard matches several serving checks ({}): refused",
                names.into_iter().collect::<Vec<_>>().join(", ")
            ))),
        }
    }

    /// The latest runs, by gate name.
    pub fn runs(&self) -> Vec<GateRun> {
        self.runs
            .read()
            .map(|r| r.values().cloned().collect())
            .unwrap_or_default()
    }

    /// `/v1/health` view.
    pub fn health(&self) -> Value {
        Value::Array(
            self.runs()
                .into_iter()
                .map(|r| {
                    json!({
                        "gate": r.gate,
                        "judged": r.judged,
                        "cleared": r.cleared,
                        "skipped": r.skipped,
                        "rootReadErrors": r.read_errors,
                        "at": r.at,
                        "firstRefusal": r.first_refusal,
                    })
                })
                .collect(),
        )
    }
}

/// A serving check the network knows by the bindings it covers (review
/// RESAPI-R1 F1). Compiled into every server, including plain `onyx-node`,
/// which has no gate: a shard with one of these schema ids or recipe ids
/// needs the named gate whatever its catalog entry says, so a server without
/// it refuses the shard (fail closed). Names and hashes only; the gate's code
/// stays in its sector adapter (ADR-0010).
#[derive(Clone, Copy, Debug)]
pub struct KnownGate {
    /// Gate name (`ServingGate::name`).
    pub name: &'static str,
    /// Row schema strings whose Keccak is a schema id the gate covers.
    pub schemas: &'static [&'static [u8]],
    /// `0x` recipe ids the gate covers (every committed build).
    pub recipe_ids: &'static [&'static str],
    /// Metric names the gate covers (review RESAPI-R2-1): a shard listed
    /// under one of these needs the gate whatever its binding, so a later
    /// build not listed above (another recipe id and schema id) still fails
    /// closed on a server without the gate.
    pub metrics: &'static [&'static str],
}

/// The known gated bindings. `onyx-reserve-serve`'s tests check this entry
/// against `SCHEMA_ENTITY_RESERVE` and the committed recipe id, so a new
/// reserve build fails the tests until it is listed here.
pub const KNOWN_GATES: &[KnownGate] = &[KnownGate {
    name: "btc-entity-reserve-v1",
    schemas: &[b"onyx.btc.entity-reserve-row.v1"],
    recipe_ids: &[
        // btc-entity-reserve with the supply cap (RES-8), the served build.
        "0xd0c1331f82f9a743548ca68de3693a2754ae35e6faef7e047a739ba939b49e34",
        // The first committed build (b982f59), before the cap.
        "0x356864e1ff04b08fed22373731944eb6c1ac07def9a3ce516ef0c1fbc8bc9351",
    ],
    metrics: &["btc.entity-reserve"],
}];

/// [`KNOWN_GATES`] as hashes: (name, schema ids, recipe ids).
type KnownIds = Vec<(&'static str, Vec<Hash32>, Vec<Hash32>)>;

fn known_ids() -> &'static KnownIds {
    static IDS: OnceLock<KnownIds> = OnceLock::new();
    IDS.get_or_init(|| {
        KNOWN_GATES
            .iter()
            .map(|k| {
                let recipes = k
                    .recipe_ids
                    .iter()
                    .map(|r| {
                        let mut b = [0u8; 32];
                        // A malformed constant would leave the recipe
                        // unlisted; `known_gates_are_well_formed` pins it.
                        let _ = hex::decode_to_slice(r.trim_start_matches("0x"), &mut b);
                        Hash32(b)
                    })
                    .collect();
                (
                    k.name,
                    k.schemas.iter().map(|s| keccak256(s)).collect(),
                    recipes,
                )
            })
            .collect()
    })
}

/// The known gate `meta` needs, if any ([`KNOWN_GATES`]): by its binding
/// (schema id, recipe id) or by its metric name (RESAPI-R2-1).
pub fn known_claim(meta: &ShardMeta) -> Option<&'static str> {
    known_ids()
        .iter()
        .zip(KNOWN_GATES)
        .find(|((_, schemas, recipes), k)| {
            schemas.contains(&meta.binding.schema_id)
                || (meta.binding.recipe_id != Hash32::ZERO
                    && recipes.contains(&meta.binding.recipe_id))
                || k.metrics.contains(&meta.metric.as_str())
        })
        .map(|((n, _, _), _)| *n)
}

/// Why a shard or row is not served (the API maps these to responses).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The shard names a gate this server does not have.
    Unavailable(String),
    /// No answer for the shard's current root yet.
    Pending,
    /// The gate refused the shard.
    Failed(String),
}

/// A shard a gate cleared: the gate's name and every row's clearance.
#[derive(Clone, Debug)]
pub struct Cleared {
    /// Gate name.
    pub gate: String,
    /// Rows by leaf index.
    pub rows: Arc<Vec<RowClearance>>,
}

impl Cleared {
    /// The clearance of the row keyed `key`, if the shard has one.
    pub fn by_key(&self, key: &[u8]) -> Option<&RowClearance> {
        self.rows.iter().find(|r| r.key == key)
    }

    /// The `servingCheck` field of an answer serving row `index`, or why
    /// that row is refused.
    pub fn row(&self, index: usize) -> Result<Value, String> {
        match self.rows.get(index).map(|r| &r.cross_check) {
            Some(Ok(c)) => Ok(row_json(&self.gate, c)),
            Some(Err(why)) => Err(why.clone()),
            None => Err("the serving check has no answer for this row".into()),
        }
    }
}

/// The cleared rows of `meta` at `root`: `Ok(None)` when no serving check
/// applies, `Ok(Some(..))` when one cleared the shard.
pub fn cleared(gates: &Gates, meta: &ShardMeta, root: &Hash32) -> Result<Option<Cleared>, Refusal> {
    let Some(name) = gates.needed(meta)? else {
        return Ok(None);
    };
    if !gates.by_name.contains_key(&name) {
        return Err(Refusal::Unavailable(name));
    }
    let answers = gates.answers.read().map_err(|_| Refusal::Pending)?;
    match answers.get(&(meta.round, meta.shard)) {
        Some(e) if e.gate == name && e.root == *root => match &e.verdict {
            Ok(rows) => Ok(Some(Cleared {
                gate: name,
                rows: rows.clone(),
            })),
            Err(why) => Err(Refusal::Failed(why.clone())),
        },
        _ => Err(Refusal::Pending),
    }
}

/// Run every gate over the finalized shards it needs in the catalog (roots
/// from the root source, rows and params from the store, the rows checked
/// against the root) and store the answers. Shards not final or not held
/// here are skipped (and so stay unserved).
///
/// A root read that fails (review RESAPI-R1 F3) is not "not final": the
/// shard is judged at the root of its last answer. If it has none, the run
/// is incomplete (a missing link would refuse the whole chain): the new
/// verdicts are not stored, previous answers stay for shards whose root read
/// back unchanged (or could not be read), and every other shard of the gate
/// is pending until a later run.
pub async fn refresh(state: &ApiState) -> Vec<GateRun> {
    let mut out = Vec::new();
    for (name, gate) in &state.gates.by_name {
        let metas: Vec<ShardMeta> = match state.catalog.read() {
            Ok(c) => c
                .shards()
                .filter(|m| matches!(state.gates.needed(m), Ok(Some(n)) if n == *name))
                .cloned()
                .collect(),
            Err(_) => continue,
        };
        let last_root = |m: &ShardMeta| -> Option<Hash32> {
            let a = state.gates.answers.read().ok()?;
            a.get(&(m.round, m.shard))
                .filter(|e| e.gate == *name)
                .map(|e| e.root)
        };
        let mut shards = Vec::new();
        let mut skipped = 0;
        let (mut read_errors, mut unread) = (0, 0);
        // Per shard: the root read back this run (`None`: not final; the
        // shard's last root when the read failed).
        let mut now_roots: BTreeMap<(u64, Hash32), Option<Hash32>> = BTreeMap::new();
        for m in &metas {
            let root = match state.roots.finalized(m.round, &m.shard).await {
                Ok(Some(r)) => r.root,
                Ok(None) => {
                    now_roots.insert((m.round, m.shard), None);
                    skipped += 1;
                    continue;
                }
                Err(_) => {
                    read_errors += 1;
                    match last_root(m) {
                        Some(r) => r,
                        None => {
                            unread += 1;
                            skipped += 1;
                            continue;
                        }
                    }
                }
            };
            now_roots.insert((m.round, m.shard), Some(root));
            let Some(leaves) = state
                .cas
                .rows(&root)
                .filter(|l| MerkleTree::from_rows(l).root() == root)
            else {
                skipped += 1;
                continue;
            };
            shards.push(GateShard {
                round: m.round,
                shard: m.shard,
                root,
                source: m.source.clone(),
                recipe_id: m.binding.recipe_id,
                label_set_hash: m.binding.label_set_hash,
                schema_id: m.binding.schema_id,
                params: state.cas.get(Kind::Blob, &m.binding.params_hash),
                leaves,
            });
        }
        let run = if unread == 0 {
            let mut verdicts = gate.judge(&shards);
            verdicts.resize(shards.len(), Err("the gate gave no answer".into()));
            // THREATS L9A-4: a clearance list must cover every leaf. An export
            // walks the list, not the leaves, so a gate that answered short
            // would have let uncleared rows out.
            let verdicts: Vec<Judgement> = shards
                .iter()
                .zip(verdicts)
                .map(|(s, v)| {
                    v.and_then(|rows| {
                        if rows.len() == s.leaves.len() {
                            Ok(rows)
                        } else {
                            Err(format!(
                                "the gate cleared {} of {} rows",
                                rows.len(),
                                s.leaves.len()
                            ))
                        }
                    })
                })
                .collect();
            let cleared = verdicts.iter().filter(|v| v.is_ok()).count();
            let first_refusal = verdicts.iter().find_map(|v| v.as_ref().err().cloned());
            if let Ok(mut a) = state.gates.answers.write() {
                a.retain(|_, e| e.gate != *name);
                for (s, v) in shards.iter().zip(verdicts) {
                    a.insert(
                        (s.round, s.shard),
                        Entry {
                            gate: name.clone(),
                            root: s.root,
                            verdict: v.map(Arc::new),
                        },
                    );
                }
            }
            GateRun {
                gate: name.clone(),
                judged: shards.len(),
                cleared,
                skipped,
                read_errors,
                at: (state.clock)(),
                first_refusal,
            }
        } else {
            // Incomplete: keep what is still known to hold, judge nothing.
            let mut kept = 0;
            if let Ok(mut a) = state.gates.answers.write() {
                a.retain(|k, e| {
                    if e.gate != *name {
                        return true;
                    }
                    let keep = match now_roots.get(k) {
                        Some(Some(r)) => *r == e.root,
                        Some(None) => false,
                        // Not read back (a failed read) or no longer listed.
                        None => metas.iter().any(|m| (m.round, m.shard) == *k),
                    };
                    kept += usize::from(keep && e.verdict.is_ok());
                    keep
                });
            }
            GateRun {
                gate: name.clone(),
                judged: 0,
                cleared: 0,
                skipped,
                read_errors,
                at: (state.clock)(),
                first_refusal: Some(format!(
                    "{unread} root read(s) failed for shards never judged: {kept} previous clearance(s) kept for unchanged roots, the rest pending until a later run"
                )),
            }
        };
        if let Ok(mut r) = state.gates.runs.write() {
            r.insert(name.clone(), run.clone());
        }
        out.push(run);
    }
    out
}

/// An indexer poll leads to a gate run every this many polls even when it
/// changed nothing (a root the indexer took out is then dropped too).
pub const GATE_POLL_EVERY: u64 = 20;

/// Run the gates again after the indexer's `polls`th poll when it changed
/// the catalog: shards admitted, or shards taken out (review RESAPI-R2-2: a
/// refuted link of a reserve chain must not leave the later links cleared
/// until the next timed run), and every [`GATE_POLL_EVERY`] polls. `None`
/// when no run was due (or there is no gate).
pub async fn after_poll(
    state: &ApiState,
    report: &crate::indexer::PollReport,
    polls: u64,
) -> Option<Vec<GateRun>> {
    let due = !report.admitted.is_empty()
        || !report.removed.is_empty()
        || polls.is_multiple_of(GATE_POLL_EVERY);
    if state.gates.is_empty() || !due {
        return None;
    }
    Some(refresh(state).await)
}

/// Run [`refresh`] every `every`, forever (review RESAPI-R1 F3): a listed
/// shard that finalizes after the start is judged without a restart, and a
/// failed root read is retried. `log` sees every run.
pub fn spawn_refresh(
    state: Arc<ApiState>,
    every: Duration,
    log: impl Fn(&GateRun) + Send + Sync + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            for r in refresh(&state).await {
                log(&r);
            }
        }
    })
}

/// One shard a gate cleared, as `/v1/served/{metric}` lists it (ROADMAP D3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServedShard {
    /// The shard as the catalog lists it.
    pub meta: ShardMeta,
    /// The root the gate cleared it at.
    pub root: Hash32,
}

/// The gate of `metric` and every shard of the catalog it cleared at the
/// root it judged, in snapshot order (start height, end, round): what a
/// client needs to fetch the links of a reserve chain and re-run the check
/// itself (THREATS RES-10). `None` when no gate is registered for `metric`.
/// Identifiers and ranges only; the rows stay behind their own endpoints.
pub fn served<'a>(
    gates: &Gates,
    shards: impl Iterator<Item = &'a ShardMeta>,
    metric: &str,
) -> Option<(String, Vec<ServedShard>)> {
    let name = gates.by_metric.get(metric)?.clone();
    let answers = gates.answers.read().ok()?;
    let mut out: Vec<ServedShard> = shards
        .filter(|m| matches!(gates.needed(m), Ok(Some(n)) if n == name))
        .filter_map(|m| {
            let e = answers.get(&(m.round, m.shard))?;
            (e.gate == name && e.verdict.is_ok()).then(|| ServedShard {
                meta: m.clone(),
                root: e.root,
            })
        })
        .collect();
    out.sort_by_key(|s| {
        (
            snapshot_start(&s.meta.source.snapshot),
            s.meta.as_of,
            s.meta.round,
        )
    });
    Some((name, out))
}

/// First height of a snapshot (a block's height; a document's time).
pub fn snapshot_start(s: &onyx_core::Snapshot) -> u64 {
    match s {
        onyx_core::Snapshot::Block { height, .. } => *height,
        onyx_core::Snapshot::BlockRange { from_height, .. } => *from_height,
        onyx_core::Snapshot::Document { published_unix, .. } => *published_unix,
    }
}

/// True if [`KNOWN_GATES`] lists `metric` (a gated, label-derived metric).
pub fn known_metric(metric: &str) -> bool {
    KNOWN_GATES.iter().any(|k| k.metrics.contains(&metric))
}

/// The `servingCheck` field of a one-row answer.
pub fn row_json(gate: &str, c: &CrossCheck) -> Value {
    json!({
        "gate": gate,
        "crossChecked": true,
        "basis": c.basis,
        "source": c.source,
    })
}
