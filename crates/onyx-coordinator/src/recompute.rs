//! Deterministic recompute of a shard from its pinned inputs (ADR-0018,
//! ADR-0019 loop 25). Miners, validators and settlement all run exactly this,
//! so an honest validator gets the honest miner's rows, and settlement decides
//! a disputed shard by rerunning it rather than by anyone's word (L24-F1).
//!
//! Everything is fetched by hash: the recipe, label set and cited
//! attestations from the CAS (integrity checked on read), the source bytes
//! checked against the snapshot's content hash, the registry checked against
//! the spec's registry root (`onyx_recipe_host::assemble_input`). A caller can
//! withhold inputs, which leaves a shard pending, but cannot change the result.

use crate::cas::{rows_storable, Cas, CasExt, Kind};
use onyx_core::{keccak256, Hash32, Registry, ShardSpec, StaticRegistry};
use onyx_labels::attest::{AttestCache, Attestation};
use onyx_labels::ATTESTATION_EVIDENCE;
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{assemble_input_cached, failure_leaf, Host, HostError, Outcome, RunResult};
use std::collections::BTreeSet;

/// Why a shard could not be computed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ComputeError {
    /// The shard has no recipe (not a recompute lane).
    #[error("shard has no recipe")]
    NoRecipe,
    /// Something the shard pins could not be fetched.
    #[error("not in the CAS: {0}")]
    Unavailable(String),
    /// Host refused the input or the recipe.
    #[error("host: {0}")]
    Host(HostError),
    /// The recipe ran but did not end with `Ok`.
    #[error("recipe outcome {0:?}")]
    Failed(Outcome),
    /// The rows would not fit in the store.
    #[error("rows too large to store")]
    RowsTooLarge,
}

/// Run a shard from its pinned inputs on the canonical engine and return its
/// rows (Merkle leaves). `source` is the snapshot's bytes from the caller's
/// own source (node or fixture); it must hash to the pinned content hash.
pub fn compute(
    spec: &ShardSpec,
    source: Vec<u8>,
    reg: &dyn Registry,
    cas: &dyn Cas,
    host: &Host,
    cache: &AttestCache,
) -> Result<Vec<Vec<u8>>, ComputeError> {
    let run = run_pinned(spec, source, reg, cas, host, cache)?;
    if run.outcome != Outcome::Ok {
        return Err(ComputeError::Failed(run.outcome));
    }
    if !rows_storable(&run.leaves) {
        return Err(ComputeError::RowsTooLarge);
    }
    Ok(run.leaves)
}

/// The canonical run of a shard from its pinned inputs, whatever its outcome.
fn run_pinned(
    spec: &ShardSpec,
    source: Vec<u8>,
    reg: &dyn Registry,
    cas: &dyn Cas,
    host: &Host,
    cache: &AttestCache,
) -> Result<RunResult, ComputeError> {
    let recipe = spec.recipe.ok_or(ComputeError::NoRecipe)?;
    let profile = spec.runtime.as_ref().ok_or(ComputeError::NoRecipe)?;
    let wasm = cas
        .get(Kind::Recipe, &recipe)
        .ok_or_else(|| ComputeError::Unavailable(format!("recipe {recipe}")))?;
    let labels =
        match &spec.label_set {
            None => None,
            Some(pin) => Some(cas.label_set(&pin.content_hash).ok_or_else(|| {
                ComputeError::Unavailable(format!("label set {}", pin.content_hash))
            })?),
        };
    let mut atts: Vec<Attestation> = Vec::new();
    if let Some(set) = &labels {
        for e in set
            .labels
            .iter()
            .flat_map(|l| l.evidence.iter())
            .filter(|e| e.kind == ATTESTATION_EVIDENCE)
        {
            let a = cas.attestation(&e.content_hash).ok_or_else(|| {
                ComputeError::Unavailable(format!("attestation {}", e.content_hash))
            })?;
            atts.push(a);
        }
    }
    let input = assemble_input_cached(spec, source, labels.as_ref(), &atts, reg, Some(cache))
        .map_err(ComputeError::Host)?
        .bytes;
    host.run(&recipe, &wasm, &input, profile)
        .map_err(ComputeError::Host)
}

/// Share of a shard's fuel limit (parts per million) its canonical run may use
/// before a round offers it (SER-1, SER-R1-F2). The rest is headroom: miners
/// and validators run the same bytes, but a shard near the limit leaves no room
/// for a busier block than the one measured, and an out-of-fuel run settles
/// with no data.
pub const PREFLIGHT_MAX_FUEL_PPM: u64 = 500_000;

/// True when a run that used `used` of `limit` fuel passes the preflight
/// budget (at most [`PREFLIGHT_MAX_FUEL_PPM`] of the limit). Mirrored on chain by
/// `OnyxRegistry.withinBudget` (ADR-0030); a zero limit never passes.
pub fn within_preflight_budget(used: u64, limit: u64) -> bool {
    limit != 0
        && u128::from(used) * 1_000_000 <= u128::from(limit) * u128::from(PREFLIGHT_MAX_FUEL_PPM)
}

/// Recipe failure codes a schema defines as a valid result (SER-R2-F2): a
/// shard whose canonical run ends with one of them settles as the canonical
/// failure leaf (for example a series range with no activity,
/// `SERIES_NO_ACTIVITY`). Every other failure (a recipe error not on the list,
/// a trap, bad output, output too large, out of fuel) means the shard was built
/// wrong: the preflight refuses it and miners do not commit it. Keyed by
/// (schema hash, code) so one schema's code never admits another's. The
/// coordinator crates stay sector-agnostic: adapters supply the list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FailureAllowList(BTreeSet<(Hash32, u32)>);

impl FailureAllowList {
    /// An empty list: no failure is a valid result.
    pub fn new() -> Self {
        Self::default()
    }

    /// Also allow `RecipeError(code)` for shards of `schema`.
    pub fn allow(mut self, schema: Hash32, code: u32) -> Self {
        self.0.insert((schema, code));
        self
    }

    /// True if `outcome` is a valid result for a shard of `schema`. Only a
    /// recipe's own failure code can be; engine outcomes never are.
    pub fn allows(&self, schema: &Hash32, outcome: &Outcome) -> bool {
        match outcome {
            Outcome::RecipeError(code) => self.0.contains(&(*schema, *code)),
            _ => false,
        }
    }
}

impl FromIterator<(Hash32, u32)> for FailureAllowList {
    fn from_iter<I: IntoIterator<Item = (Hash32, u32)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// Why a shard failed its preflight.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PreflightError {
    /// The coordinator has no source bytes for the shard.
    #[error("source bytes not available")]
    NoSource,
    /// The shard could not be run (inputs missing, recipe refused, ...).
    #[error("preflight run failed: {0}")]
    Compute(ComputeError),
    /// The run used more than [`PREFLIGHT_MAX_FUEL_PPM`] of the fuel limit, or
    /// ran out of fuel.
    #[error("fuel {used} over {max_ppm} ppm of the limit {limit}")]
    OverBudget {
        /// Fuel the canonical run used.
        used: u64,
        /// The shard's fuel limit.
        limit: u64,
        /// The allowed share, in ppm.
        max_ppm: u64,
    },
    /// The canonical run ended with a failure that is not a valid result for
    /// the shard's schema (SER-R2-F2): the shard was built wrong.
    #[error("canonical run failed with {0:?}, not a valid result for this schema")]
    RefusedFailure(Outcome),
    /// The shard's source is an estimate (`estimate.` namespace): a checked
    /// value never has an estimate among its inputs (R-EST-2, ADR-0050).
    #[error("source {0} is an estimate, not a checked input")]
    EstimateInput(String),
}

/// Run a shard once on the canonical engine before its round opens and refuse
/// it when the run uses more than [`PREFLIGHT_MAX_FUEL_PPM`] of its fuel limit
/// (SER-R1-F2). Returns the fuel used. Only `Ok` or a failure `allowed` lists
/// for the shard's schema passes (SER-R2-F2, e.g. a series range with no
/// activity); running out of fuel, a trap, bad output or any other recipe
/// error never does.
pub fn preflight(
    spec: &ShardSpec,
    source: Vec<u8>,
    reg: &dyn Registry,
    cas: &dyn Cas,
    host: &Host,
    cache: &AttestCache,
    allowed: &FailureAllowList,
) -> Result<u64, PreflightError> {
    if spec.source.source.is_estimate() {
        return Err(PreflightError::EstimateInput(spec.source.source.0.clone()));
    }
    let limit = spec
        .runtime
        .as_ref()
        .ok_or(PreflightError::Compute(ComputeError::NoRecipe))?
        .fuel_limit;
    let run = run_pinned(spec, source, reg, cas, host, cache).map_err(PreflightError::Compute)?;
    if run.outcome == Outcome::OutOfFuel || !within_preflight_budget(run.fuel_used, limit) {
        return Err(PreflightError::OverBudget {
            used: run.fuel_used,
            limit,
            max_ppm: PREFLIGHT_MAX_FUEL_PPM,
        });
    }
    if run.outcome != Outcome::Ok && !allowed.allows(&spec.schema, &run.outcome) {
        return Err(PreflightError::RefusedFailure(run.outcome));
    }
    Ok(run.fuel_used)
}

/// What a deterministic rerun says about a shard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reference {
    /// The shard's true root: of its rows, or of the canonical failure leaf
    /// when the recipe deterministically fails.
    Root(Hash32),
    /// No root can be correct: the canonical engine cannot load the recipe,
    /// or its rows cannot be stored. Every reveal is wrong.
    NoValidRoot,
    /// This machine could not decide (inputs withheld or not the pinned ones,
    /// engine abstained, host bug). Not a verdict: the shard stays pending.
    Undecided(String),
}

/// Fixed marker recorded for [`Reference::NoValidRoot`]. No revealed root can
/// equal it: it is not the Merkle root of any rows.
pub fn no_valid_root_marker(shard: &Hash32) -> Hash32 {
    let mut b = b"onyx-no-valid-root-v1".to_vec();
    b.extend_from_slice(&shard.0);
    keccak256(&b)
}

/// Classify a compute result. Only outcomes every honest machine with the
/// same host build reaches give a reference; anything that depends on the
/// caller's inputs or machine is undecided.
pub fn classify(result: Result<Vec<Vec<u8>>, ComputeError>) -> Reference {
    match result {
        Ok(rows) => Reference::Root(MerkleTree::from_rows(&rows).root()),
        Err(ComputeError::Failed(o)) => {
            Reference::Root(MerkleTree::from_rows(&[failure_leaf(o)]).root())
        }
        Err(ComputeError::RowsTooLarge) => Reference::NoValidRoot,
        // Load-time rejections after the profile check passed (same host
        // build, so same limits): properties of the recipe bytes alone.
        Err(ComputeError::Host(
            HostError::Invalid(_)
            | HostError::ForbiddenImport(_)
            | HostError::BadAbi(_)
            | HostError::FrameTooLarge(_)
            | HostError::TooLarge(_)
            | HostError::EngineRejected(_),
        )) => Reference::NoValidRoot,
        Err(e) => Reference::Undecided(e.to_string()),
    }
}

/// What settlement needs to rerun a shard. The registry is the concrete
/// static registry (its root is computed from its contents, not reported by a
/// trait object), the host is the canonical engine, and the CAS checks
/// integrity on every read. `source` returns the snapshot bytes for a spec.
pub struct RecomputeEnv<'a> {
    /// Registry the specs pin (root checked per spec).
    pub reg: &'a StaticRegistry,
    /// Content-addressed store holding the pinned inputs.
    pub cas: &'a dyn Cas,
    /// Canonical engine.
    pub host: &'a Host,
    /// Source bytes for a spec's snapshot (checked against its content hash).
    pub source: &'a dyn Fn(&ShardSpec) -> Option<Vec<u8>>,
}

impl RecomputeEnv<'_> {
    /// Rerun `spec` and classify the result.
    pub fn reference(&self, spec: &ShardSpec, cache: &AttestCache) -> Reference {
        match (self.source)(spec) {
            None => Reference::Undecided("source bytes not available".into()),
            Some(bytes) => classify(compute(spec, bytes, self.reg, self.cas, self.host, cache)),
        }
    }

    /// [`preflight`] with this environment's inputs.
    pub fn preflight(
        &self,
        spec: &ShardSpec,
        cache: &AttestCache,
        allowed: &FailureAllowList,
    ) -> Result<u64, PreflightError> {
        let bytes = (self.source)(spec).ok_or(PreflightError::NoSource)?;
        preflight(spec, bytes, self.reg, self.cas, self.host, cache, allowed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixture::World;

    /// R-EST-2 (ADR-0050): a recipe whose source is an estimate is refused
    /// before anything runs.
    #[test]
    fn preflight_refuses_an_estimate_input() {
        let w = World::new();
        let mut spec = w.spec(1);
        spec.source.source = onyx_core::SourceId("estimate.model.output".into());
        let r = preflight(
            &spec,
            vec![1, 2, 3],
            &w.reg,
            &w.cas,
            &w.host,
            &AttestCache::default(),
            &FailureAllowList::new(),
        );
        assert_eq!(
            r,
            Err(PreflightError::EstimateInput(
                "estimate.model.output".into()
            ))
        );
    }

    #[test]
    fn only_listed_recipe_errors_are_valid_results() {
        let schema = keccak256(b"schema a");
        let other = keccak256(b"schema b");
        let list = FailureAllowList::new().allow(schema, 8);
        assert!(list.allows(&schema, &Outcome::RecipeError(8)));
        assert!(!list.allows(&schema, &Outcome::RecipeError(5)));
        assert!(!list.allows(&other, &Outcome::RecipeError(8)));
        for o in [
            Outcome::Ok,
            Outcome::OutOfFuel,
            Outcome::Trap,
            Outcome::OutputTooLarge,
            Outcome::BadOutput,
        ] {
            assert!(!list.allows(&schema, &o), "{o:?}");
        }
        assert!(!FailureAllowList::new().allows(&schema, &Outcome::RecipeError(8)));
    }

    #[test]
    fn only_deterministic_outcomes_decide() {
        assert!(matches!(classify(Ok(vec![vec![1; 8]])), Reference::Root(_)));
        assert!(matches!(
            classify(Err(ComputeError::Failed(Outcome::Trap))),
            Reference::Root(_)
        ));
        assert_eq!(
            classify(Err(ComputeError::RowsTooLarge)),
            Reference::NoValidRoot
        );
        assert_eq!(
            classify(Err(ComputeError::Host(HostError::ForbiddenImport(
                "clock".into()
            )))),
            Reference::NoValidRoot
        );
        for undecided in [
            ComputeError::Unavailable("recipe".into()),
            ComputeError::NoRecipe,
            ComputeError::Host(HostError::BadSource),
            ComputeError::Host(HostError::Abstain("oom".into())),
            ComputeError::Host(HostError::WrongProfile("other build".into())),
            ComputeError::Host(HostError::BadLabels("registry".into())),
            ComputeError::Host(HostError::IdMismatch(Hash32::ZERO)),
            ComputeError::Host(HostError::Internal("bug".into())),
        ] {
            assert!(matches!(classify(Err(undecided)), Reference::Undecided(_)));
        }
    }
}
