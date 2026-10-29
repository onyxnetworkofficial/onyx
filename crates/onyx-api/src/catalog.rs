//! What the API serves: shard metadata from the specs it was settled under,
//! the rights of its source, metrics by name, prediction-market bindings, and
//! the time-decay price (docs/spec/pricing.md).

use crate::observations::{subject_text, ObservationFeed};
use onyx_core::{
    is_estimate_id, keccak256, shard_id, CommitmentFields, DataRights, Estimate, Hash32,
    MarketPolicy, ProofLevel, ShardSpec, Snapshot, SourceRef,
};
use std::collections::BTreeMap;

/// Every input of the on-chain binding hash except the root and the row count,
/// plus what the shard id is derived from. A client recomputes the shard id
/// and the binding from these, so recipe, runtime, label set, source, schema
/// and rights are checkable, not claims (M7-F2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShardBinding {
    /// Recipe id or zero.
    pub recipe_id: Hash32,
    /// Runtime profile hash or zero.
    pub runtime_hash: Hash32,
    /// `LabelSetRef::hash` or zero.
    pub label_set_hash: Hash32,
    /// `SourceRef::hash`.
    pub source_hash: Hash32,
    /// Row schema id.
    pub schema_id: Hash32,
    /// `DataRights::hash`.
    pub rights_hash: Hash32,
    /// `keccak256(params)`.
    pub params_hash: Hash32,
    /// Registry root the shard is judged against.
    pub registry_root: Hash32,
}

impl ShardBinding {
    /// The binding inputs of `spec`.
    pub fn from_spec(spec: &ShardSpec) -> Self {
        let f = onyx_core::Commitment {
            root: Hash32::ZERO,
            leaf_count: 0,
            recipe: spec.recipe,
            runtime: spec.runtime.clone(),
            label_set: spec.label_set.clone(),
            source: spec.source.clone(),
            schema: spec.schema,
            rights_hash: spec.rights_hash,
        }
        .fields();
        ShardBinding {
            recipe_id: f.recipe_id,
            runtime_hash: f.runtime_hash,
            label_set_hash: f.label_set_hash,
            source_hash: f.source_hash,
            schema_id: f.schema_id,
            rights_hash: f.rights_hash,
            params_hash: keccak256(&spec.params),
            registry_root: spec.registry_root,
        }
    }

    /// The commitment fields for `root` with `leaf_count` rows.
    pub fn fields(&self, root: Hash32, leaf_count: u64) -> CommitmentFields {
        CommitmentFields {
            root,
            leaf_count,
            recipe_id: self.recipe_id,
            runtime_hash: self.runtime_hash,
            label_set_hash: self.label_set_hash,
            source_hash: self.source_hash,
            schema_id: self.schema_id,
            rights_hash: self.rights_hash,
        }
    }

    /// The shard id these inputs derive (equals `ShardSpec::shard_id`).
    pub fn shard_id(&self) -> Hash32 {
        shard_id(
            &self.fields(Hash32::ZERO, 0).shard_fields_hash(),
            &self.params_hash,
            &self.registry_root,
        )
    }

    /// The binding hash the contract records for `root` with `leaf_count` rows.
    pub fn binding(&self, root: Hash32, leaf_count: u64) -> Hash32 {
        self.fields(root, leaf_count).binding_hash()
    }
}

/// Static facts about a shard, taken from its spec and the registry when the
/// API learns of it. The root itself always comes from a `RootSource`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShardMeta {
    /// Round.
    pub round: u64,
    /// Shard id.
    pub shard: Hash32,
    /// Metric name (e.g. `btc.entity-flow`).
    pub metric: String,
    /// Height (or document time) the data describes up to, for `as_of` queries.
    /// Always [`snapshot_end`] of `source.snapshot` (M7-L40-F1): a client
    /// derives it from the source reference inside the binding, so it is not a
    /// claim of the operator.
    pub as_of: u64,
    /// The pinned source and snapshot (its hash is `binding.source_hash`). Sent
    /// in the verification bundle so a client can bind `asOf` (M7-L40-F1).
    pub source: SourceRef,
    /// The spec's binding inputs.
    pub binding: ShardBinding,
    /// Row count of the finalized commitment, when the indexer knows it. Lets
    /// the server price and range-check before reading any row (M7-F3); a
    /// wrong value is caught because the binding hashes it.
    pub leaf_count: Option<u64>,
    /// The pinned label set is a test set (`test.*`): refused unless the
    /// server runs in dev mode.
    pub test_labels: bool,
    /// Rights of the source, checked at serve time.
    pub rights: DataRights,
    /// How strongly the rows are checked (docs/spec/proof-ladder.md): the
    /// weakest of the shard's inputs (L2R2-F3). The recipe rerun is level 1;
    /// a pinned label set has its own level (signed by the entity: 3; a
    /// published list: 4; scored judgment: 5), and rerunning the recipe does
    /// not check the labels. A market settles on a shard only if the
    /// catalog's [`MarketPolicy`] accepts this level (L2R-F8); the check is
    /// advisory until the level is bound on chain (ADR-0027 amendment 2).
    pub proof_level: ProofLevel,
    /// The weakest of the shard's inputs other than its label set: the
    /// recipe rerun (level 1) and every [`ShardMeta::with_input_level`]. A
    /// label level supplied later never undoes it.
    pub input_floor: ProofLevel,
    /// The pinned label set's level, when the caller supplied it
    /// ([`ShardMeta::with_label_set_level`]); `None` for a shard with no label
    /// set or one whose level this server does not know (then
    /// `proof_level` is the fail-closed level 5 until a pinned registry
    /// attests one, [`ShardMeta::with_attested_level`]).
    pub label_level: Option<ProofLevel>,
    /// The weakest level a pinned registry attested for this shard
    /// ([`ShardMeta::with_attested_level`]); `None` before any. Kept so the
    /// level is a function of its inputs, whatever order they arrive in: a
    /// later, stronger attestation or a supplied label level never lifts the
    /// shard above the chain's level (review L16-R1).
    pub attested_level: Option<ProofLevel>,
    /// Unix time of the data's end (AUD-2-F2): a document's `published_unix`
    /// (bound in the snapshot), or for a block snapshot the end block's header
    /// time, which the catalog's builder reads from the header the snapshot's
    /// tip hash commits to (a client checks it the same way). The price clock
    /// never starts later than this plus [`MAX_DATA_LAG_SECS`], so a round
    /// over old blocks is not sold as fresh whenever the coordinator settles
    /// it. `None` (a block snapshot whose builder gave no time) keeps the
    /// finalization clock; `onyx-node` refuses that for block snapshots.
    pub snapshot_unix: Option<u64>,
    /// The serving check this shard needs before any row is served
    /// (ADR-0034 Amendment 1, the `gate` module): a server without a gate of
    /// that name refuses the shard (fail closed). `None`: the metric's gate,
    /// if the server registered one, else none.
    pub serving_check: Option<String>,
}

/// The most a shard's price clock may start after its data's end (AUD-2-F2):
/// one day covers commit, reveal, validation and the challenge window of a
/// normal round.
pub const MAX_DATA_LAG_SECS: u64 = 86_400;

impl ShardMeta {
    /// Metadata for `spec` in `round`; `rights` must be the registry's record for
    /// `spec.source.source` (its hash equals `spec.rights_hash`). `as_of` is the
    /// end of the pinned snapshot ([`snapshot_end`]), never an operator value.
    /// `None` too for an `estimate.` metric: a checked shard is never an
    /// estimate (R-EST-2, ADR-0050).
    pub fn from_spec(
        round: u64,
        spec: &ShardSpec,
        metric: impl Into<String>,
        rights: DataRights,
    ) -> Option<Self> {
        let metric = metric.into();
        if rights.hash() != spec.rights_hash || is_estimate_id(&metric) {
            return None;
        }
        Some(ShardMeta {
            round,
            shard: spec.shard_id(),
            metric,
            as_of: snapshot_end(&spec.source.snapshot),
            source: spec.source.clone(),
            binding: ShardBinding::from_spec(spec),
            leaf_count: None,
            test_labels: spec
                .label_set
                .as_ref()
                .is_some_and(|l| l.name.starts_with("test.")),
            rights,
            // Every ShardSpec names a recipe (level 1). A pinned label set's
            // level is not known here: fail closed to the weakest level until
            // the caller supplies it (`with_label_set_level`, L2R2-F3).
            proof_level: if spec.label_set.is_some() {
                ProofLevel::TrackRecord
            } else {
                ProofLevel::Recomputed
            },
            input_floor: ProofLevel::Recomputed,
            label_level: None,
            attested_level: None,
            snapshot_unix: match &spec.source.snapshot {
                onyx_core::Snapshot::Document { published_unix, .. } => Some(*published_unix),
                _ => None,
            },
            serving_check: None,
        })
    }

    /// Require the serving check `gate` before any row of this shard is
    /// served (fail closed when the server has no such gate).
    pub fn with_serving_check(mut self, gate: impl Into<String>) -> Self {
        self.serving_check = Some(gate.into());
        self
    }

    /// Set the end time of a block snapshot (the end block's header time,
    /// read from the header its tip hash commits to). A document snapshot's
    /// time is its bound `published_unix` and is not replaced.
    pub fn with_snapshot_unix(mut self, unix: u64) -> Self {
        if self.snapshot_unix.is_none() {
            self.snapshot_unix = Some(unix);
        }
        self
    }

    /// When this shard's price clock starts, given its root finalized at
    /// `finalized_unix` (AUD-2-F2): the finalization, but never later than
    /// the data's end plus [`MAX_DATA_LAG_SECS`].
    pub fn clock_start(&self, finalized_unix: u64) -> u64 {
        match self.snapshot_unix {
            Some(t) => finalized_unix.min(t.saturating_add(MAX_DATA_LAG_SECS)),
            None => finalized_unix,
        }
    }

    /// Set the level of the shard's pinned label set, from the label
    /// registry or the set's reviewed provenance. The shard's level becomes
    /// the weakest of the recipe rerun (1) and the label set (L2R2-F3). A
    /// shard with no label set stays level 1 and `level` is ignored.
    pub fn with_label_set_level(mut self, level: ProofLevel) -> Self {
        if self.labelled() {
            self.label_level = Some(level);
            self.relevel();
        }
        self
    }

    /// Weaken the shard's level to `level` for an input the recipe rerun
    /// does not check (AUD-2-F6): e.g. a pinned mapping table taken from a
    /// third-party list. Never strengthens it.
    pub fn with_input_level(mut self, level: ProofLevel) -> Self {
        self.input_floor = self.input_floor.weakest(level);
        self.relevel();
        self
    }

    /// Apply `level`, the level a pinned registry attested for this shard
    /// (`OnyxRegistry.levelOf`, after its code hash and its ledger were
    /// checked, AUD-2R-F3 and L16). The registry refused an attestation
    /// stronger than the label set's registered level or the recipe's floor
    /// (ADR-0030), so for a label set whose level this server does not know
    /// the chain's level stands in for it: the shard keeps its attested level
    /// instead of the fail-closed level 5. Otherwise it only weakens: never
    /// above an input's floor ([`ShardMeta::with_input_level`]) or a label
    /// level the caller supplied.
    pub fn with_attested_level(mut self, level: ProofLevel) -> Self {
        self.attest(level);
        self
    }

    fn attest(&mut self, level: ProofLevel) {
        self.attested_level = Some(match self.attested_level {
            Some(a) => a.weakest(level),
            None => level,
        });
        self.relevel();
    }

    /// `proof_level` from its inputs (L2R2-F3, L16): the weakest of the
    /// input floor, the label set's level (supplied, else the chain's
    /// attested level standing in for it, else the fail-closed 5) and the
    /// chain's attested level. Order-independent, and an attestation only
    /// ever lowers it below what the other inputs allow.
    fn relevel(&mut self) {
        let mut level = self.input_floor;
        if self.labelled() {
            level = level.weakest(
                self.label_level
                    .or(self.attested_level)
                    .unwrap_or(ProofLevel::TrackRecord),
            );
        }
        if let Some(a) = self.attested_level {
            level = level.weakest(a);
        }
        self.proof_level = level;
    }

    fn labelled(&self) -> bool {
        self.binding.label_set_hash != Hash32::ZERO
    }

    /// Recipe that produced the rows, if any.
    pub fn recipe_id(&self) -> Option<Hash32> {
        (self.binding.recipe_id != Hash32::ZERO).then_some(self.binding.recipe_id)
    }

    /// True if the rows are totals derived from an entity label set (company
    /// flows, balances, reserves), whose redistribution rights are not cleared
    /// yet (THREATS L8-5, ROADMAP L14). A server in [`LabelRights::Fallback`]
    /// serves none of them. Fail closed: any one of these marks it,
    /// - the binding pins a label set (`label_set_hash` is not zero; a spec
    ///   without one hashes to zero, e.g. the mining-pool shards);
    /// - a known serving check claims it ([`crate::gate::known_claim`]: the
    ///   reserve gate's schema ids, recipe ids and metric names);
    /// - a segment of its metric name starts with `entity`
    ///   (`btc.entity-flow`, `crypto.btc.entity_reserve`), so a misnamed
    ///   binding under an entity metric is withheld too.
    pub fn label_derived(&self) -> bool {
        self.binding.label_set_hash != Hash32::ZERO
            || crate::gate::known_claim(self).is_some()
            || self
                .metric
                .split(['.', '/'])
                .any(|seg| seg.to_ascii_lowercase().starts_with("entity"))
    }
}

/// Whether the entity label sets' redistribution rights are cleared
/// (`ONYX_RIGHTS`, ROADMAP L14, THREATS L8-5). `Cleared` (the default) serves
/// every shard as before; `Fallback` refuses every label-derived shard
/// ([`ShardMeta::label_derived`]) with 403 `label_rights_withheld`, before
/// any 402, so nothing is charged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LabelRights {
    /// Rights cleared: serve label-derived shards.
    #[default]
    Cleared,
    /// Rights not cleared: serve no label-derived shard.
    Fallback,
}

impl LabelRights {
    /// Parse `cleared` or `fallback` (exact, lowercase: a near miss such as
    /// `Fallback` or `true` is an error, never a silent default; cf. L9A-6).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "cleared" => Ok(LabelRights::Cleared),
            "fallback" => Ok(LabelRights::Fallback),
            other => Err(format!(
                "ONYX_RIGHTS must be cleared or fallback, not {other:?}"
            )),
        }
    }

    /// The name `/v1/health` reports.
    pub fn as_str(self) -> &'static str {
        match self {
            LabelRights::Cleared => "cleared",
            LabelRights::Fallback => "fallback",
        }
    }
}

/// What a snapshot describes up to: the last height of a block range, the
/// height of a block, or a document's publication time. Shared by the server
/// (`asOf`) and the client check, so both derive it the same way.
pub fn snapshot_end(s: &Snapshot) -> u64 {
    match s {
        Snapshot::Block { height, .. } => *height,
        Snapshot::BlockRange { to_height, .. } => *to_height,
        Snapshot::Document { published_unix, .. } => *published_unix,
    }
}

/// A prediction market bound to one row of one shard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Market {
    /// Round.
    pub round: u64,
    /// Shard.
    pub shard: Hash32,
    /// Subject whose row settles the market (the row key, e.g. `entity:okx`).
    pub subject: String,
}

/// Step-function decay: `base` while fresh, free after `fresh_window_secs`.
/// Age runs from the root's finalization time, capped at the data's end plus
/// [`MAX_DATA_LAG_SECS`] ([`ShardMeta::clock_start`], AUD-2-F2), never from a
/// time the operator sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PricingPolicy {
    /// Price per call (per row for an export) in micro-USDC.
    pub base_price_micro_usdc: u64,
    /// Seconds a row stays paid.
    pub fresh_window_secs: u64,
}

impl PricingPolicy {
    /// Price of one row whose data is `age_secs` old.
    pub fn price(&self, age_secs: u64) -> u64 {
        if age_secs < self.fresh_window_secs {
            self.base_price_micro_usdc
        } else {
            0
        }
    }
}

/// Recommended price of one fresh answer (one row with its proof), in
/// micro-USDC: 0.25 cents (ADR-0025 item 4). Operators set their own; the
/// website states this one from fixtures/revenue/pricing-defaults.json.
pub const RECOMMENDED_ANSWER_MICRO_USDC: u64 = 2_500;

/// Flat price of a whole dataset (an export) while fresh (ADR-0025 item 4):
/// AI agents pay once per dataset instead of once per row. Never more than
/// buying every row, never less than `floor_rows` rows (so a dataset is not a
/// cheaper way to buy a handful of rows than `/v1/proofs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatasetPricing {
    /// Price of one fresh dataset in micro-USDC.
    pub flat_micro_usdc: u64,
    /// The flat price is at least this many rows at the per-row price.
    pub floor_rows: u64,
}

impl Default for DatasetPricing {
    /// 2 cents per dataset, at least 4 rows' worth (ADR-0025 defaults).
    fn default() -> Self {
        DatasetPricing {
            flat_micro_usdc: 20_000,
            floor_rows: 4,
        }
    }
}

impl DatasetPricing {
    /// Price of a dataset of `rows` rows whose single row costs `per_row`
    /// (already decayed: 0 when old, so old datasets stay free):
    /// `min(rows * per_row, max(flat, min(rows, floor_rows) * per_row))`.
    pub fn price(&self, per_row: u64, rows: u64) -> u64 {
        if per_row == 0 {
            return 0;
        }
        let all = per_row.saturating_mul(rows);
        let floor = per_row.saturating_mul(rows.min(self.floor_rows));
        all.min(self.flat_micro_usdc.max(floor))
    }
}

/// Everything the API knows that is not a root or a row.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    shards: BTreeMap<(u64, Hash32), ShardMeta>,
    markets: BTreeMap<String, Market>,
    pricing: BTreeMap<String, PricingPolicy>,
    datasets: BTreeMap<String, DatasetPricing>,
    /// Policy for metrics without their own.
    pub default_pricing: Option<PricingPolicy>,
    /// Dataset policy for metrics without their own; `None` prices an export
    /// per row (the pre-ADR-0025 rule).
    pub default_dataset_pricing: Option<DatasetPricing>,
    /// Proof levels a market may settle on; default: the money policy, levels
    /// 1 and 3 (ADR-0027, L2R-F8).
    pub market_policy: MarketPolicy,
    /// Level-2 metrics served by `/v1/observations` (ADR-0027 amendment 4),
    /// by metric name. Never bound to a market: markets bind shard rows only,
    /// and level 2 is outside the money policy.
    observations: BTreeMap<String, ObservationFeed>,
    /// Estimates served by `/v1/estimates` (ADR-0050), by metric then subject
    /// text: the newest estimate of each subject. Apart from every checked
    /// map above, so no checked route, total or market can reach one.
    estimates: BTreeMap<String, BTreeMap<String, Estimate>>,
}

impl Catalog {
    /// Empty catalog (everything free until a policy is set).
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace a shard. A shard of an `estimate.` metric is never
    /// added (R-EST-2): [`ShardMeta::from_spec`] already refuses one, so this
    /// only stops a hand-built [`ShardMeta`].
    pub fn add_shard(&mut self, meta: ShardMeta) {
        if is_estimate_id(&meta.metric) {
            return;
        }
        self.shards.insert((meta.round, meta.shard), meta);
    }

    /// Take shard (round, `shard`) out (the chain indexer, when two reads
    /// refute the view it was admitted on, IDX-R5-2).
    pub fn remove_shard(&mut self, round: u64, shard: &Hash32) -> Option<ShardMeta> {
        self.shards.remove(&(round, *shard))
    }

    /// Every shard's (round, shard id), in order.
    pub fn shard_keys(&self) -> Vec<(u64, Hash32)> {
        self.shards.keys().copied().collect()
    }

    /// Apply `level`, the level the pinned registry attested on chain for
    /// shard (round, `shard`) (AUD-2R-F3, L16; [`ShardMeta::with_attested_level`]):
    /// it weakens the shard, and stands in only for a label set level this
    /// server does not know. False when the catalog has no such shard.
    pub fn floor_shard_level(&mut self, round: u64, shard: &Hash32, level: ProofLevel) -> bool {
        match self.shards.get_mut(&(round, *shard)) {
            Some(m) => {
                m.attest(level);
                true
            }
            None => false,
        }
    }

    /// How many shards the catalog serves at each proof level (`/v1/health`).
    pub fn level_counts(&self) -> std::collections::BTreeMap<u8, usize> {
        let mut n = std::collections::BTreeMap::new();
        for m in self.shards.values() {
            *n.entry(m.proof_level.number()).or_default() += 1;
        }
        n
    }

    /// Bind market `id` to a row.
    pub fn add_market(&mut self, id: impl Into<String>, m: Market) {
        self.markets.insert(id.into(), m);
    }

    /// Set the pricing of a metric.
    pub fn set_pricing(&mut self, metric: impl Into<String>, p: PricingPolicy) {
        self.pricing.insert(metric.into(), p);
    }

    /// Set the dataset (export) pricing of a metric.
    pub fn set_dataset_pricing(&mut self, metric: impl Into<String>, p: DatasetPricing) {
        self.datasets.insert(metric.into(), p);
    }

    /// Serve a level-2 metric. Refuses a second feed for a metric, and a
    /// metric name a shard metric already uses (one name, one kind of answer).
    pub fn add_observations(&mut self, feed: ObservationFeed) -> Result<(), String> {
        let metric = feed.metric().to_owned();
        if is_estimate_id(&metric) {
            return Err(format!(
                "metric {metric:?} is an estimate, not a checked value"
            ));
        }
        if self.observations.contains_key(&metric)
            || self.shards.values().any(|m| m.metric == metric)
        {
            return Err(format!("metric {metric:?} is already served"));
        }
        self.observations.insert(metric, feed);
        Ok(())
    }

    /// Serve estimates (ADR-0050). Each must pass [`Estimate::validate`],
    /// so its metric is in the `estimate.` namespace (a checked metric is
    /// refused); a newer estimate of a subject replaces an older one, an older
    /// one is ignored.
    pub fn add_estimates(&mut self, estimates: Vec<Estimate>) -> Result<(), String> {
        for e in &estimates {
            e.validate()
                .map_err(|err| format!("estimate {:?}: {err}", e.metric.0))?;
        }
        for e in estimates {
            let by_subject = self.estimates.entry(e.metric.0.clone()).or_default();
            let key = subject_text(&e.subject).to_owned();
            if by_subject.get(&key).is_none_or(|old| old.as_of < e.as_of) {
                by_subject.insert(key, e);
            }
        }
        Ok(())
    }

    /// The newest estimate of `metric` about `subject` (its `?subject=` text).
    pub fn estimate(&self, metric: &str, subject: &str) -> Option<&Estimate> {
        self.estimates.get(metric)?.get(subject)
    }

    /// True if `metric` has any estimate.
    pub fn has_estimates(&self, metric: &str) -> bool {
        self.estimates.contains_key(metric)
    }

    /// The level-2 feed of `metric`.
    pub fn observations(&self, metric: &str) -> Option<&ObservationFeed> {
        self.observations.get(metric)
    }

    /// The level-2 feed of `metric`, to add a round a paid observation ran (ADR-0049; the round
    /// is checked by [`ObservationFeed::add`]).
    pub(crate) fn observations_mut(&mut self, metric: &str) -> Option<&mut ObservationFeed> {
        self.observations.get_mut(metric)
    }

    /// Price of one observation of `metric` settled at `settled_unix`, asked
    /// at `now`: the metric's (or the default) per-row policy, decaying from
    /// its template-fixed decay start (`FeedTemplate::decay_start`).
    pub fn observation_price(&self, metric: &str, settled_unix: u64, now: u64) -> u64 {
        self.pricing
            .get(metric)
            .or(self.default_pricing.as_ref())
            .map_or(0, |p| p.price(now.saturating_sub(settled_unix)))
    }

    /// Every shard listed, by (round, shard).
    pub fn shards(&self) -> impl Iterator<Item = &ShardMeta> {
        self.shards.values()
    }

    /// Shard metadata.
    pub fn shard(&self, round: u64, shard: &Hash32) -> Option<&ShardMeta> {
        self.shards.get(&(round, *shard))
    }

    /// Market binding.
    pub fn market(&self, id: &str) -> Option<&Market> {
        self.markets.get(id)
    }

    /// True if a market may settle on rows of `meta` (its proof level is one
    /// the market policy accepts). The settlement endpoint refuses otherwise.
    pub fn settles_on(&self, meta: &ShardMeta) -> bool {
        self.market_policy.accepts(meta.proof_level)
    }

    /// Shards of `metric` with `as_of <= limit` (or all), newest first.
    pub fn for_metric(&self, metric: &str, limit: Option<u64>) -> Vec<&ShardMeta> {
        let mut v: Vec<&ShardMeta> = self
            .shards
            .values()
            .filter(|m| m.metric == metric && limit.is_none_or(|l| m.as_of <= l))
            .collect();
        v.sort_by_key(|m| std::cmp::Reverse((m.as_of, m.round)));
        v
    }

    /// Price of `rows` rows of `meta` whose root finalized at unix time
    /// `finalized_unix`, asked at unix time `now` (saturating). The age runs
    /// from [`ShardMeta::clock_start`]: never later than the data's end plus
    /// a day (AUD-2-F2).
    pub fn price(&self, meta: &ShardMeta, finalized_unix: u64, now: u64, rows: u64) -> u64 {
        let start = meta.clock_start(finalized_unix);
        self.pricing
            .get(&meta.metric)
            .or(self.default_pricing.as_ref())
            .map_or(0, |p| {
                p.price(now.saturating_sub(start)).saturating_mul(rows)
            })
    }

    /// Price of exporting all `rows` rows of `meta`: the metric's (or the
    /// default) [`DatasetPricing`] over the decayed per-row price, else per row.
    pub fn export_price(&self, meta: &ShardMeta, finalized_unix: u64, now: u64, rows: u64) -> u64 {
        let per_row = self.price(meta, finalized_unix, now, 1);
        match self
            .datasets
            .get(&meta.metric)
            .or(self.default_dataset_pricing.as_ref())
        {
            Some(d) => d.price(per_row, rows),
            None => per_row.saturating_mul(rows),
        }
    }
}

#[cfg(test)]
mod tests {
    /// The website's run page states the prices from
    /// fixtures/revenue/pricing-defaults.json; this keeps that file equal to
    /// the defaults here (AUD-2-F11). Regenerate with
    /// `ONYX_BLESS=1 cargo test -p onyx-api pricing_defaults_file`.
    #[test]
    fn pricing_defaults_file_matches_the_defaults() {
        let d = DatasetPricing::default();
        let doc = serde_json::json!({
            "answerMicroUsdc": RECOMMENDED_ANSWER_MICRO_USDC,
            "datasetMicroUsdc": d.flat_micro_usdc,
            "datasetFloorRows": d.floor_rows,
        });
        let text = format!("{}\n", serde_json::to_string_pretty(&doc).expect("json"));
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/revenue/pricing-defaults.json"
        );
        if std::env::var_os("ONYX_BLESS").is_some() {
            std::fs::write(path, &text).expect("write");
        }
        let on_disk = std::fs::read_to_string(path)
            .expect("fixtures/revenue/pricing-defaults.json missing; run with ONYX_BLESS=1");
        assert_eq!(on_disk, text, "pricing defaults file drifted");
    }

    use super::*;

    fn level_spec(labelled: bool) -> (ShardSpec, DataRights) {
        let src = onyx_core::SourceId("chain.test".into());
        let spec = ShardSpec {
            recipe: None,
            runtime: None,
            label_set: labelled.then(|| onyx_core::LabelSetRef {
                name: "onyx.entities.test".into(),
                version: 1,
                content_hash: onyx_core::keccak256(b"labels"),
            }),
            source: SourceRef {
                source: src.clone(),
                snapshot: onyx_core::Snapshot::Block {
                    chain: onyx_core::ChainId("test".into()),
                    height: 10,
                    block_hash: onyx_core::keccak256(b"b"),
                },
            },
            schema: onyx_core::keccak256(b"schema"),
            rights_hash: DataRights::public_chain(src.clone()).hash(),
            params: vec![],
            registry_root: onyx_core::keccak256(b"r"),
        };
        (spec, DataRights::public_chain(src))
    }

    /// L16 (THREATS L2R2-F2): with a pinned, checked registry the chain's
    /// attested level stands in for a pinned label set's level the server
    /// could not know (`from_spec` fails closed to level 5), so a node keeps
    /// the shard's attested level. It never lifts a shard above an input's
    /// floor or a label level the caller supplied, and lowers any other.
    #[test]
    fn an_attested_level_stands_in_for_an_unknown_label_level() {
        use ProofLevel::*;
        let meta = |labelled: bool| {
            let (spec, rights) = level_spec(labelled);
            ShardMeta::from_spec(1, &spec, "test.flow", rights).expect("meta")
        };
        // A labelled shard, label level unknown: level 5 until attested.
        let m = meta(true);
        assert_eq!(m.proof_level, TrackRecord);
        assert_eq!(
            m.clone().with_attested_level(BondedClaim).proof_level,
            BondedClaim
        );
        assert_eq!(
            m.clone().with_attested_level(TrackRecord).proof_level,
            TrackRecord
        );
        // Twice is the same as once.
        assert_eq!(
            m.clone()
                .with_attested_level(BondedClaim)
                .with_attested_level(BondedClaim)
                .proof_level,
            BondedClaim
        );
        // An input's floor stays (AUD-2-F6), before or after the label level.
        let weak = m.clone().with_input_level(TrackRecord);
        assert_eq!(
            weak.with_attested_level(BondedClaim).proof_level,
            TrackRecord
        );
        let four = meta(true).with_input_level(BondedClaim);
        assert_eq!(
            four.clone().with_attested_level(Recomputed).proof_level,
            BondedClaim
        );
        // A supplied label level is never lifted by the chain.
        let known = meta(true).with_label_set_level(SourceProven);
        assert_eq!(known.proof_level, SourceProven);
        assert_eq!(
            known.clone().with_attested_level(Recomputed).proof_level,
            SourceProven
        );
        assert_eq!(
            known.with_attested_level(BondedClaim).proof_level,
            BondedClaim
        );
        // An input level set before the label level is kept too.
        let early = meta(true)
            .with_input_level(BondedClaim)
            .with_label_set_level(Recomputed);
        assert_eq!(early.proof_level, BondedClaim);
        // No label set: the chain only lowers (a pool shard's recipe floor).
        let plain = meta(false);
        assert_eq!(plain.proof_level, Recomputed);
        assert_eq!(
            plain.clone().with_attested_level(BondedClaim).proof_level,
            BondedClaim
        );
        let floored = plain.with_input_level(BondedClaim);
        assert_eq!(
            floored.with_attested_level(Recomputed).proof_level,
            BondedClaim
        );
    }

    /// Review L16-R4 (money): under the money default (levels 1 and 3), a
    /// labelled catalog-file shard never settled on onyx-node (fail-closed
    /// 5). With the registry pin it settles exactly when the chain attested
    /// level 3 (or 1) for it, which the registry allows only for a label set
    /// the owner registered at 3 or better; today's label sets are level 4,
    /// so nothing more settles. An input floor still blocks it.
    #[test]
    fn a_labelled_shard_settles_money_only_at_an_attested_accepted_level() {
        use ProofLevel::*;
        let c = Catalog::new();
        let (spec, rights) = level_spec(true);
        let m = ShardMeta::from_spec(1, &spec, "test.flow", rights).expect("meta");
        assert!(!c.settles_on(&m), "no pin: level 5");
        assert!(c.settles_on(&m.clone().with_attested_level(SourceProven)));
        assert!(!c.settles_on(&m.clone().with_attested_level(BondedClaim)));
        assert!(!c.settles_on(&m.clone().with_attested_level(TrackRecord)));
        assert!(!c.settles_on(
            &m.with_input_level(BondedClaim)
                .with_attested_level(SourceProven)
        ));
    }

    /// Review L16-R1: the level is a function of its inputs, in any order.
    /// For every sequence of up to three inputs (an input level, a supplied
    /// label level, an attested level), labelled or not: the shard is never
    /// stronger than any input level, any attested level or a supplied label
    /// level; a labelled shard with neither a label level nor an attestation
    /// is 5; and an attestation alone stands in for the label level. Before
    /// the fix, a second, stronger attestation (or a label level supplied
    /// after one) lifted the shard above the chain's level.
    #[test]
    fn the_level_never_beats_an_input_in_any_order() {
        use ProofLevel::*;
        #[derive(Clone, Copy)]
        enum Op {
            Input(ProofLevel),
            Label(ProofLevel),
            Attest(ProofLevel),
        }
        let ops: Vec<Op> = ProofLevel::ALL
            .iter()
            .flat_map(|l| [Op::Input(*l), Op::Label(*l), Op::Attest(*l)])
            .collect();
        let mut seqs: Vec<Vec<Op>> = vec![vec![]];
        for _ in 0..3 {
            let next: Vec<Vec<Op>> = seqs
                .iter()
                .filter(|q| q.len() == seqs.iter().map(Vec::len).max().unwrap_or(0))
                .flat_map(|q| {
                    ops.iter().map(move |o| {
                        let mut q = q.clone();
                        q.push(*o);
                        q
                    })
                })
                .collect();
            seqs.extend(next);
        }
        assert_eq!(seqs.len(), 1 + 15 + 225 + 3375);
        for labelled in [false, true] {
            let (spec, rights) = level_spec(labelled);
            let base = ShardMeta::from_spec(1, &spec, "test.flow", rights).expect("meta");
            for q in &seqs {
                let mut m = base.clone();
                let (mut floor, mut label, mut chain) = (1u8, None::<u8>, None::<u8>);
                for o in q {
                    m = match *o {
                        Op::Input(l) => {
                            floor = floor.max(l.number());
                            m.with_input_level(l)
                        }
                        Op::Label(l) => {
                            if labelled {
                                label = Some(l.number());
                            }
                            m.with_label_set_level(l)
                        }
                        Op::Attest(l) => {
                            chain = Some(chain.unwrap_or(0).max(l.number()));
                            m.with_attested_level(l)
                        }
                    };
                }
                let got = m.proof_level.number();
                assert!(got >= floor, "beats an input level");
                assert!(got >= chain.unwrap_or(1), "beats the chain's level");
                if labelled {
                    assert!(got >= label.unwrap_or(1), "beats the label level");
                    if label.is_none() && chain.is_none() {
                        assert_eq!(m.proof_level, TrackRecord, "fail closed");
                    }
                }
                let want = floor.max(chain.unwrap_or(1)).max(if labelled {
                    label.or(chain).unwrap_or(5)
                } else {
                    1
                });
                assert_eq!(got, want, "labelled {labelled}");
            }
        }
        // The two orders that lifted before the fix.
        let (spec, rights) = level_spec(true);
        let m = ShardMeta::from_spec(1, &spec, "test.flow", rights).expect("meta");
        assert_eq!(
            m.clone()
                .with_attested_level(BondedClaim)
                .with_attested_level(Recomputed)
                .proof_level,
            BondedClaim
        );
        assert_eq!(
            m.with_attested_level(TrackRecord)
                .with_label_set_level(Recomputed)
                .proof_level,
            TrackRecord
        );
    }

    /// AUD-2-F2: a round over old data is not sold as fresh because it was
    /// finalized late: the clock starts at the data's end plus a day at the
    /// latest, and a block snapshot's time cannot be replaced once set by a
    /// document's bound `published_unix`.
    #[test]
    fn the_price_clock_starts_no_later_than_the_data_plus_a_day() {
        let spec = ShardSpec {
            recipe: None,
            runtime: None,
            label_set: None,
            source: SourceRef {
                source: onyx_core::SourceId("chain.test".into()),
                snapshot: onyx_core::Snapshot::Block {
                    chain: onyx_core::ChainId("test".into()),
                    height: 10,
                    block_hash: onyx_core::keccak256(b"b"),
                },
            },
            schema: onyx_core::keccak256(b"schema"),
            rights_hash: DataRights::public_chain(onyx_core::SourceId("chain.test".into())).hash(),
            params: vec![],
            registry_root: onyx_core::keccak256(b"r"),
        };
        let rights = DataRights::public_chain(onyx_core::SourceId("chain.test".into()));
        let m = ShardMeta::from_spec(1, &spec, "test.flow", rights).expect("meta");
        // No time given: the finalization clock (onyx-node refuses this).
        assert_eq!(m.clock_start(5_000_000), 5_000_000);
        let m = m.with_snapshot_unix(1_000_000);
        // Finalized a week after the data: the clock started a day after it.
        assert_eq!(
            m.clock_start(1_000_000 + 7 * 86_400),
            1_000_000 + MAX_DATA_LAG_SECS
        );
        // Finalized within the day: the finalization time.
        assert_eq!(m.clock_start(1_000_500), 1_000_500);
        let mut c = Catalog::new();
        c.default_pricing = Some(PricingPolicy {
            base_price_micro_usdc: 100,
            fresh_window_secs: 3_600,
        });
        // A week-late finalization of week-old data is free, not fresh.
        let late = 1_000_000 + 7 * 86_400;
        assert_eq!(c.price(&m, late, late + 10, 1), 0);
        assert_eq!(c.price(&m, 1_000_500, 1_000_600, 1), 100);
    }

    #[test]
    fn dataset_price_is_flat_between_the_floor_and_every_row() {
        let d = DatasetPricing::default();
        // 0.25 cents a row: a large dataset costs the flat 2 cents.
        assert_eq!(d.price(2_500, 1_000), 20_000);
        // Never more than buying every row.
        assert_eq!(d.price(2_500, 3), 7_500);
        // Never less than the floor: 4 rows of 10 cents beat the flat price.
        assert_eq!(d.price(100_000, 1_000), 400_000);
        // Old data (per-row 0) stays free.
        assert_eq!(d.price(0, 1_000), 0);
        assert_eq!(d.price(u64::MAX, u64::MAX), u64::MAX);
    }

    #[test]
    fn price_steps_to_free_after_the_window() {
        let p = PricingPolicy {
            base_price_micro_usdc: 1_000,
            fresh_window_secs: 3_600,
        };
        assert_eq!(p.price(0), 1_000);
        assert_eq!(p.price(3_599), 1_000);
        assert_eq!(p.price(3_600), 0);
        assert_eq!(p.price(u64::MAX), 0);
    }
}
