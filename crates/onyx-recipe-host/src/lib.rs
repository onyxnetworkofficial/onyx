//! Deterministic recipe runner (ADR-0004, ADR-0013, ADR-0014).
//!
//! The canonical engine is the **wasmi** interpreter (`Host`). Its call stack
//! and value stack live on the heap with explicit limits, so stack exhaustion
//! is a deterministic trap that depends only on the wasm and the pinned
//! profile, never on native frame sizes, CPU or compiler (R-HOST-STACK-2).
//! Fuel is counted on wasm bytecode (deterministic). Every committed result and
//! every dispute verdict comes from this engine.
//!
//! `fast::FastHost` (wasmtime, optimizer off) is an advisory fast path only.
//!
//! Load-time checks (both engines), in order:
//! 1. `keccak256(wasm) == recipe id`; runtime profile supported (`check_profile`).
//! 2. Module and function size caps (compile cost is not metered).
//! 3. Validation with an explicit feature allowlist: no floats (scalar or SIMD),
//!    no SIMD, threads, memory64, exceptions, GC or tail calls.
//! 4. No imports at all; ABI exports present; bounded tables; frame slots
//!    (params + locals + max operand height) bounded per function.
//!
//! Every failure maps to one canonical failure leaf, so failing runs also
//! commit byte-identical roots.

use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::{keccak256, Hash32, RecipeId, Registry, RuntimeProfile, ShardSpec, Snapshot};
use onyx_labels::attest::{AttestCache, AttestContext, Attestation};
use onyx_labels::{DropReason, LabelError, LabelSet};
use onyx_merkle::MerkleTree;
use onyx_recipe_sdk::{
    unpack_ptr_len, RecipeInput, RecipeOutput, RunReturn, ABI_VERSION, EXPORT_ABI_VERSION,
    EXPORT_ALLOC, EXPORT_RUN,
};
use std::collections::HashMap;
use std::sync::Mutex;
use wasmparser::{Parser, Payload, ValidPayload, Validator, WasmFeatures};

#[cfg(feature = "fast")]
pub mod fast;

/// Host build id. Must equal `RuntimeProfile.host` for a run to be accepted,
/// so a dispute always recomputes with the same engine build (R-DRAW-1, R-DET-4).
/// Names the canonical engine: wasmi 2.0.0 with eager translation and
/// portable dispatch.
pub const HOST_ID: &str = "onyx-host.0.5.0.wasmi.2.0.0";

/// Maximum frame slots per function: params + locals + the deepest operand
/// stack the validator sees anywhere in the body. Bounds wasmi's value-stack
/// use per frame, and native frames on the advisory fast path.
pub const MAX_FRAME_SLOTS: u32 = 2048;

/// Largest call depth a profile may pin.
pub const MAX_CALL_DEPTH: u32 = 1024;

/// wasmi value-stack ceiling in bytes (8-byte cells): enough for the deepest
/// allowed recursion of the largest allowed frames. Fixed, so it is part of
/// what `HOST_ID` pins.
/// Sized for `MAX_CALL_DEPTH + 1` frames (the entry frame counts in wasmi)
/// of `MAX_FRAME_SLOTS`, times 2 for wasmi's own per-frame extras such as
/// constants (L9-F1). Frames beyond this budget trap deterministically.
pub const WASMI_MAX_STACK_BYTES: usize =
    (MAX_CALL_DEPTH as usize + 1) * MAX_FRAME_SLOTS as usize * 8 * 2;

/// Largest output a profile may allow (the host allocates it before reading).
pub const MAX_OUTPUT_BYTES: u32 = 16 << 20;
/// Most functions per module. The reference recipe has 234.
pub const MAX_FUNCTIONS: u32 = 8192;
/// Deepest block/loop/if nesting per function. The reference recipe uses 18.
/// Deep nesting of multi-value blocks made the fast path's compiler use more
/// than 6 GiB (L9-F2).
pub const MAX_BLOCK_NESTING: u32 = 256;
/// Most blocks/loops/ifs of any kind per function (defence in depth for the
/// fast path's compiler, L10-F1). The reference recipe uses at most 50.
pub const MAX_BLOCKS_PER_FUNCTION: u32 = 4096;
/// Most blocks/loops/ifs with a function-type block type (block parameters)
/// per function. The fast path's compiler memory grows about quadratically in
/// these (10,240 used 3.3 GB); plain blocks are cheap. The reference recipe uses 0.
pub const MAX_TYPED_BLOCKS_PER_FUNCTION: u32 = 64;

/// Maximum table elements (declared or grown). `table.grow` is charged fuel only
/// after allocating, so the limiter must stop it first (R-HOST-TABLE-1).
pub const MAX_TABLE_ELEMENTS: u64 = 65_536;

/// Ceiling on a profile's memory limit, so no pinned profile can exhaust a validator.
pub const MAX_MEMORY_BYTES: u64 = 1 << 30;

/// Module size cap and per-function body cap: translation is not fuel-metered,
/// so its cost is bounded by input size instead (R-HOST-COMPILE-1/2; measured in
/// docs/ops/benchmarks.md).
pub const MAX_MODULE_BYTES: usize = 1 << 20;
/// Most `br_table` targets per function, summed over all `br_table`s. Validation
/// and translation cost grows with targets x label arity (L8-COMPILE-3). The
/// reference recipe uses at most 25 per table.
pub const MAX_BR_TABLE_TARGETS_PER_FUNCTION: usize = 4096;
/// Most results per function type (bounds every label arity, since block types
/// are function types or single values). The reference recipe uses at most 1.
pub const MAX_TYPE_RESULTS: usize = 16;
/// Most params per function type. The reference recipe uses at most 7.
pub const MAX_TYPE_PARAMS: usize = 64;
/// Largest declared initial memory. Initial memory is zero-filled at every
/// instantiation without fuel, so it is capped separately (L8-OOM-1).
pub const MAX_INITIAL_MEMORY_BYTES: u64 = 64 << 20;

/// Per-function body cap.
pub const MAX_FUNCTION_BYTES: usize = 256 << 10;

/// Load-time rejection. These mean "this recipe is not runnable", not a run result.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// Bytes hash to a different id than requested.
    #[error("recipe bytes do not hash to {0}")]
    IdMismatch(RecipeId),
    /// Validation failed: floats, SIMD, threads or another disallowed feature, or malformed.
    #[error("recipe rejected by validator: {0}")]
    Invalid(String),
    /// Module imports something (clock, random, network, anything).
    #[error("recipe imports {0}; ABI v1 allows no imports")]
    ForbiddenImport(String),
    /// Required export missing, wrong type, or wrong ABI version.
    #[error("bad ABI: {0}")]
    BadAbi(String),
    /// A function's frame (params + locals + max operand stack) is too large.
    #[error("function {0} needs more than {MAX_FRAME_SLOTS} frame slots")]
    FrameTooLarge(u32),
    /// Module, function body or table too large.
    #[error("too large: {0}")]
    TooLarge(String),
    /// The pinned runtime profile names another host build or an unsupported limit.
    #[error("runtime profile not supported by this host: {0}")]
    WrongProfile(String),
    /// Source bytes do not match the snapshot's content hash (L14-F10).
    #[error("source bytes do not match the shard snapshot")]
    BadSource,
    /// The label set handed to a recipe is not the pinned, valid one (R-LBLVIEW-1).
    #[error("label set rejected: {0}")]
    BadLabels(String),
    /// The canonical engine (wasmi) rejects the module although our checks passed.
    #[error("canonical engine rejects recipe: {0}")]
    EngineRejected(String),
    /// This machine could not complete the run (out of system memory, engine
    /// panic). Not a verdict: another validator must decide.
    #[error("abstain, not a verdict: {0}")]
    Abstain(String),
    /// The advisory fast path only runs allowlisted (reviewed) recipe ids.
    #[error("recipe {0} is not on the fast-path allowlist")]
    NotAllowlisted(RecipeId),
    /// Engine or instrumentation failure (a host bug, not a recipe verdict).
    #[error("internal host error: {0}")]
    Internal(String),
}

/// How a run ended. Everything except `Ok` is committed as one canonical failure leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Outcome {
    /// Rows produced.
    Ok,
    /// Fuel exhausted.
    OutOfFuel,
    /// Trap: panic, unreachable, call depth exceeded, out of memory, bad memory access.
    Trap,
    /// Output larger than `max_output_bytes`.
    OutputTooLarge,
    /// Output not decodable, or rows not strictly ascending by key.
    BadOutput,
    /// Recipe returned its own failure code.
    RecipeError(u32),
}

/// Canonical failure leaf: `borsh(("onyx-failure-v1", outcome))`.
pub fn failure_leaf(outcome: Outcome) -> Vec<u8> {
    borsh::to_vec(&("onyx-failure-v1", outcome)).unwrap_or_default()
}

/// Result of one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunResult {
    /// How it ended.
    pub outcome: Outcome,
    /// Merkle leaves: Borsh of each `OutputRow`, or the single failure leaf.
    pub leaves: Vec<Vec<u8>>,
    /// Fuel consumed. Reported, never committed.
    pub fuel_used: u64,
}

impl RunResult {
    /// Merkle root over the leaves (ADR-0003).
    pub fn root(&self) -> Hash32 {
        MerkleTree::from_rows(&self.leaves).root()
    }
}

/// What [`assemble_input`] built: the Borsh `RecipeInput` and the labels it
/// left out (address and reason), for logs and for the miner's own check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssembledInput {
    /// Borsh `RecipeInput`.
    pub bytes: Vec<u8>,
    /// Labels dropped by the attestation check.
    pub dropped: Vec<(String, DropReason)>,
}

/// Build the Borsh `RecipeInput` for a shard from the shard spec, the registry
/// it pins, and data whose hashes the spec pins, so parties holding the same
/// data cannot differ (L13-F3, L14-F1):
/// 1. the spec must validate against `reg` (registry root, source rights), and
///    the source bytes must hash to the snapshot's `content_hash` (block-range
///    and document snapshots; a single-block snapshot is refused, L15-F2);
/// 2. the label set must be exactly the pinned one (checked first: it is cheap,
///    L13-F5) and validate against the registry;
/// 3. attestations are judged over the shard's own block range, mapped to an
///    address namespace by the registry's proof rule for that chain;
/// 4. labels whose attestation fails, and full-confidence labels without one,
///    are dropped; the recipe sees the rest (ADR-0015 item 9). A cited
///    attestation the caller does not hold is an error: abstain, do not guess.
///    Snapshots without a block range (documents) cannot judge attestations,
///    so attested labels are dropped there.
pub fn assemble_input(
    spec: &ShardSpec,
    source: Vec<u8>,
    labels: Option<&LabelSet>,
    attestations: &[Attestation],
    reg: &dyn Registry,
) -> Result<AssembledInput, HostError> {
    assemble_input_cached(spec, source, labels, attestations, reg, None)
}

/// As [`assemble_input`], reusing attestation signature verdicts across shards
/// from `cache` (L15-F4). Results are identical with or without the cache.
pub fn assemble_input_cached(
    spec: &ShardSpec,
    source: Vec<u8>,
    labels: Option<&LabelSet>,
    attestations: &[Attestation],
    reg: &dyn Registry,
    cache: Option<&AttestCache>,
) -> Result<AssembledInput, HostError> {
    spec.validate(reg)
        .map_err(|e| HostError::BadLabels(format!("shard spec: {e}")))?;
    // Source bytes must be exactly the pinned content (L14-F10, L15-F2). A
    // single-block snapshot pins only a block hash, not the adapter's bytes, so
    // it cannot feed a recipe.
    match &spec.source.snapshot {
        Snapshot::BlockRange { content_hash, .. } | Snapshot::Document { content_hash, .. } => {
            if keccak256(&source) != *content_hash {
                return Err(HostError::BadSource);
            }
        }
        _ => return Err(HostError::BadSource),
    }
    let (labels, dropped) = match (labels, spec.label_set.as_ref()) {
        (None, None) => (Vec::new(), Vec::new()),
        (Some(set), Some(pin)) => {
            if set.name != pin.name
                || set.version != pin.version
                || set.content_hash() != pin.content_hash
            {
                return Err(HostError::BadLabels("not the pinned label set".into()));
            }
            set.validate(reg)
                .map_err(|e| HostError::BadLabels(e.to_string()))?;
            let range = snapshot_range(&spec.source.snapshot, reg);
            let ranges: Vec<(&str, u64, u64)> = range.into_iter().collect();
            let ctx = AttestContext { ranges: &ranges };
            let (kept, dropped) = set
                .attested_subset_cached(attestations, &ctx, reg, cache)
                .map_err(|e| match e {
                    // This machine lacks data the set pins: no verdict (L14-F1).
                    LabelError::AttestationMissing(a) => {
                        HostError::Abstain(format!("cited attestation not held for {a}"))
                    }
                    LabelError::SuppliedTooLarge => {
                        HostError::Abstain("caller supplied too many attestation bytes".into())
                    }
                    other => HostError::BadLabels(other.to_string()),
                })?;
            let bytes = borsh::to_vec(&kept).map_err(|e| HostError::Internal(e.to_string()))?;
            (bytes, dropped)
        }
        _ => {
            return Err(HostError::BadLabels(
                "label set presence differs from the pin".into(),
            ))
        }
    };
    let bytes = borsh::to_vec(&RecipeInput {
        abi_version: ABI_VERSION,
        source,
        labels,
        params: spec.params.clone(),
    })
    .map_err(|e| HostError::Internal(e.to_string()))?;
    Ok(AssembledInput { bytes, dropped })
}

/// The address namespace and block range a snapshot covers, if the registry
/// has a proof rule for its chain.
fn snapshot_range<'r>(snapshot: &Snapshot, reg: &'r dyn Registry) -> Option<(&'r str, u64, u64)> {
    let (chain, from, to) = match snapshot {
        Snapshot::BlockRange {
            chain,
            from_height,
            to_height,
            ..
        } => (chain, *from_height, *to_height),
        Snapshot::Block { chain, height, .. } => (chain, *height, *height),
        _ => return None,
    };
    reg.proof_for_chain(&chain.0)
        .map(|p| (p.namespace(), from, to))
}

/// Feature allowlist. Everything else, including floats, is rejected.
pub(crate) fn allowed_features() -> WasmFeatures {
    WasmFeatures::MUTABLE_GLOBAL
        | WasmFeatures::SIGN_EXTENSION
        | WasmFeatures::MULTI_VALUE
        | WasmFeatures::BULK_MEMORY
        | WasmFeatures::REFERENCE_TYPES
}

/// Check a runtime profile against what this host supports. The only profile
/// check; used on both the cold and the cached path (R-PROFILE-1).
pub fn check_profile(profile: &RuntimeProfile) -> Result<(), HostError> {
    if profile.host != HOST_ID {
        return Err(HostError::WrongProfile(profile.host.clone()));
    }
    if profile.abi_version != ABI_VERSION {
        return Err(HostError::WrongProfile(format!(
            "abi {}",
            profile.abi_version
        )));
    }
    if profile.max_call_depth == 0 || profile.max_call_depth > MAX_CALL_DEPTH {
        return Err(HostError::WrongProfile(format!(
            "call depth {}",
            profile.max_call_depth
        )));
    }
    if profile.max_output_bytes > MAX_OUTPUT_BYTES {
        return Err(HostError::WrongProfile(format!(
            "output {}",
            profile.max_output_bytes
        )));
    }
    if profile.max_memory_bytes > MAX_MEMORY_BYTES {
        return Err(HostError::WrongProfile(format!(
            "memory {}",
            profile.max_memory_bytes
        )));
    }
    Ok(())
}

/// Full validation with the feature allowlist, plus: no imports, ABI exports,
/// bounded tables, bounded function sizes and frame slots. Returns the number
/// of defined globals.
pub(crate) fn static_checks(wasm: &[u8]) -> Result<u32, HostError> {
    let inv = |e: wasmparser::BinaryReaderError| HostError::Invalid(e.to_string());
    let mut validator = Validator::new_with_features(allowed_features());
    let mut globals = 0u32;
    let mut have = [false; 4];
    let mut func_index = 0u32;
    for payload in Parser::new(0).parse_all(wasm) {
        let payload = payload.map_err(inv)?;
        match &payload {
            Payload::ImportSection(r) if r.count() > 0 => {
                return Err(HostError::ForbiddenImport(format!(
                    "{} import(s)",
                    r.count()
                )));
            }
            Payload::GlobalSection(r) => globals = r.count(),
            Payload::TypeSection(r) => {
                for ft in r.clone().into_iter_err_on_gc_types() {
                    let ft = ft.map_err(inv)?;
                    if ft.params().len() > MAX_TYPE_PARAMS || ft.results().len() > MAX_TYPE_RESULTS
                    {
                        return Err(HostError::TooLarge("function type arity".into()));
                    }
                }
            }
            Payload::FunctionSection(r) if r.count() > MAX_FUNCTIONS => {
                return Err(HostError::TooLarge(format!("{} functions", r.count())));
            }
            Payload::MemorySection(r) => {
                for m in r.clone() {
                    let m = m.map_err(inv)?;
                    if m.initial.saturating_mul(65_536) > MAX_INITIAL_MEMORY_BYTES {
                        return Err(HostError::TooLarge("initial memory".into()));
                    }
                }
            }
            Payload::TableSection(r) => {
                for t in r.clone() {
                    let t = t.map_err(inv)?;
                    if t.ty.initial > MAX_TABLE_ELEMENTS
                        || t.ty.maximum.is_some_and(|m| m > MAX_TABLE_ELEMENTS)
                    {
                        return Err(HostError::TooLarge("table".into()));
                    }
                }
            }
            Payload::ExportSection(r) => {
                for e in r.clone() {
                    let e = e.map_err(inv)?;
                    match e.name {
                        n if n == EXPORT_ALLOC => have[0] = true,
                        n if n == EXPORT_RUN => have[1] = true,
                        n if n == EXPORT_ABI_VERSION => have[2] = true,
                        "memory" => have[3] = true,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        if let ValidPayload::Func(to_validate, body) = validator.payload(&payload).map_err(inv)? {
            let range = body.range();
            if range.end - range.start > MAX_FUNCTION_BYTES as u64 {
                return Err(HostError::TooLarge(format!("function {func_index}")));
            }
            // Cheap pass first: bound br_table work and nesting before the costly validation.
            let mut targets = 0usize;
            let mut depth = 0u32;
            let mut typed_blocks = 0u32;
            let mut blocks = 0u32;
            let mut ops = body.get_operators_reader().map_err(inv)?;
            while !ops.eof() {
                match ops.read().map_err(inv)? {
                    wasmparser::Operator::BrTable { targets: t } => {
                        targets = targets.saturating_add(t.len() as usize + 1);
                        if targets > MAX_BR_TABLE_TARGETS_PER_FUNCTION {
                            return Err(HostError::TooLarge(format!(
                                "br_table targets in function {func_index}"
                            )));
                        }
                    }
                    wasmparser::Operator::Block { blockty }
                    | wasmparser::Operator::Loop { blockty }
                    | wasmparser::Operator::If { blockty } => {
                        blocks += 1;
                        if blocks > MAX_BLOCKS_PER_FUNCTION {
                            return Err(HostError::TooLarge(format!(
                                "blocks in function {func_index}"
                            )));
                        }
                        if matches!(blockty, wasmparser::BlockType::FuncType(_)) {
                            typed_blocks += 1;
                            if typed_blocks > MAX_TYPED_BLOCKS_PER_FUNCTION {
                                return Err(HostError::TooLarge(format!(
                                    "typed blocks in function {func_index}"
                                )));
                            }
                        }
                        depth += 1;
                        if depth > MAX_BLOCK_NESTING {
                            return Err(HostError::TooLarge(format!(
                                "block nesting in function {func_index}"
                            )));
                        }
                    }
                    wasmparser::Operator::End => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            let mut v = to_validate.into_validator(Default::default());
            let mut reader = body.get_binary_reader();
            v.read_locals(&mut reader).map_err(inv)?;
            if v.len_locals() > MAX_FRAME_SLOTS {
                return Err(HostError::FrameTooLarge(func_index));
            }
            let mut max_height = 0u32;
            while !reader.eof() {
                let pos = reader.original_position();
                reader
                    .visit_operator(&mut v.visitor(pos))
                    .map_err(inv)?
                    .map_err(inv)?;
                max_height = max_height.max(v.operand_stack_height());
            }
            reader
                .finish_expression(&v.visitor(reader.original_position()))
                .map_err(inv)?;
            if u64::from(v.len_locals()) + u64::from(max_height) > u64::from(MAX_FRAME_SLOTS) {
                return Err(HostError::FrameTooLarge(func_index));
            }
            func_index += 1;
        }
    }
    if have != [true; 4] {
        return Err(HostError::BadAbi(
            "needs exports onyx_alloc, onyx_run, onyx_abi_version, memory".into(),
        ));
    }
    Ok(globals)
}

pub(crate) fn fail(outcome: Outcome, fuel_used: u64) -> Result<RunResult, HostError> {
    Ok(RunResult {
        outcome,
        leaves: vec![failure_leaf(outcome)],
        fuel_used,
    })
}

/// Turn raw `onyx_run` output into a run result: size check, decode,
/// non-empty and strictly ascending keys, one leaf per row. Shared by every engine.
pub(crate) fn decode_output(
    out: &[u8],
    profile: &RuntimeProfile,
    fuel_used: u64,
) -> Result<RunResult, HostError> {
    if out.len() as u64 > u64::from(profile.max_output_bytes) {
        return fail(Outcome::OutputTooLarge, fuel_used);
    }
    let rows = match borsh::from_slice::<RunReturn>(out) {
        Ok(RunReturn::Ok(RecipeOutput { rows })) => rows,
        Ok(RunReturn::Err(code)) => return fail(Outcome::RecipeError(code), fuel_used),
        Err(_) => return fail(Outcome::BadOutput, fuel_used),
    };
    // At least one row (a commitment has >= 1 leaf), keys strictly ascending (R-DUPKEY-1).
    if rows.is_empty() || !rows.windows(2).all(|w| w[0].key < w[1].key) {
        return fail(Outcome::BadOutput, fuel_used);
    }
    let leaves = rows
        .iter()
        .map(|r| borsh::to_vec(r).map_err(|e| HostError::Internal(e.to_string())))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RunResult {
        outcome: Outcome::Ok,
        leaves,
        fuel_used,
    })
}

/// Most compiled modules kept in the cache.
pub const CACHE_MAX_ENTRIES: usize = 32;
/// Most source bytes (sum of cached wasm sizes) kept in the cache; compiled
/// size grows with source size, so this bounds cache memory (R-CACHE-2).
pub const CACHE_MAX_SOURCE_BYTES: usize = 32 << 20;

/// Bounded least-recently-used cache of compiled modules, keyed by (recipe id,
/// call depth), with an entry cap and a source-byte budget (R-CACHE-2, L8-CACHE-6).
pub(crate) struct ModuleCache<T: Clone> {
    entries: HashMap<(RecipeId, u32), (T, usize, u64)>,
    clock: u64,
    bytes: usize,
}

impl<T: Clone> Default for ModuleCache<T> {
    fn default() -> Self {
        ModuleCache {
            entries: HashMap::new(),
            clock: 0,
            bytes: 0,
        }
    }
}

impl<T: Clone> ModuleCache<T> {
    pub(crate) fn get(&mut self, key: &(RecipeId, u32)) -> Option<T> {
        self.clock += 1;
        let now = self.clock;
        self.entries.get_mut(key).map(|e| {
            e.2 = now;
            e.0.clone()
        })
    }

    pub(crate) fn insert(&mut self, key: (RecipeId, u32), module: T, source_len: usize) {
        self.clock += 1;
        while !self.entries.is_empty()
            && (self.entries.len() >= CACHE_MAX_ENTRIES
                || self.bytes + source_len > CACHE_MAX_SOURCE_BYTES)
        {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, _, used))| *used)
                .map(|(k, _)| *k);
            match oldest.and_then(|k| self.entries.remove(&k)) {
                Some((_, len, _)) => self.bytes -= len,
                None => break,
            }
        }
        self.bytes += source_len;
        self.entries.insert(key, (module, source_len, self.clock));
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Canonical engine: wasmi interpreter. Translated modules are cached per
/// (recipe id, call depth) in a bounded LRU.
pub struct Host {
    cache: Mutex<ModuleCache<(wasmi::Engine, wasmi::Module)>>,
}

/// Alias kept for callers: the canonical cache uses the shared limits.
pub const HOST_CACHE_MAX_ENTRIES: usize = CACHE_MAX_ENTRIES;

fn wasmi_engine(max_call_depth: u32) -> wasmi::Engine {
    let mut c = wasmi::Config::default();
    c.consume_fuel(true)
        .floats(false)
        // Lazy translation charges fuel on first use and caches per engine,
        // so a second run would pay less: never for verdicts.
        .compilation_mode(wasmi::CompilationMode::Eager)
        // wasmi counts the entry frame (`onyx_run`) too; `max_call_depth` counts
        // nested calls below it, as the fast path's counter does (L8-DEPTH-1).
        .set_max_recursion_depth(max_call_depth as usize + 1)
        .set_max_stack_height(WASMI_MAX_STACK_BYTES)
        .wasm_multi_memory(false)
        .wasm_tail_call(false)
        .wasm_extended_const(false)
        .wasm_custom_page_sizes(false)
        .wasm_wide_arithmetic(false)
        .wasm_saturating_float_to_int(false);
    wasmi::Engine::new(&c)
}

/// Translate `wasm` with the canonical engine, inside a panic guard. A
/// translation failure caused by this machine (out of memory) or a panic
/// abstains; anything else is a deterministic rejection (L9-F4).
fn translate(engine: &wasmi::Engine, wasm: &[u8]) -> Result<wasmi::Module, HostError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wasmi::Module::new(engine, wasm)
    })) {
        Ok(Ok(m)) => Ok(m),
        Ok(Err(e)) if format!("{e:?}").contains("OutOfSystemMemory") => Err(HostError::Abstain(
            format!("out of system memory while translating: {e}"),
        )),
        Ok(Err(e)) => Err(HostError::EngineRejected(e.to_string())),
        Err(_) => Err(HostError::Abstain(
            "engine panicked while translating".into(),
        )),
    }
}

/// True if the canonical engine accepts the module: full translation, the same
/// step `run` performs, so `prepare` and `run` agree (L8-VALID-4, L9-F4).
pub fn canonical_accepts(wasm: &[u8]) -> Result<(), HostError> {
    translate(&wasmi_engine(MAX_CALL_DEPTH), wasm).map(|_| ())
}

/// Declared initial linear memory in bytes (0 if none).
pub(crate) fn initial_memory_bytes(wasm: &[u8]) -> u64 {
    for p in Parser::new(0).parse_all(wasm) {
        if let Ok(Payload::MemorySection(r)) = p {
            return r
                .into_iter()
                .flatten()
                .map(|m| m.initial.saturating_mul(65_536))
                .max()
                .unwrap_or(0);
        }
    }
    0
}

impl Host {
    /// Build the canonical host.
    pub fn new() -> Result<Host, HostError> {
        Ok(Host {
            cache: Mutex::new(ModuleCache::default()),
        })
    }

    fn checks(
        &self,
        id: &RecipeId,
        wasm: &[u8],
        profile: &RuntimeProfile,
    ) -> Result<(), HostError> {
        if keccak256(wasm) != *id {
            return Err(HostError::IdMismatch(*id));
        }
        check_profile(profile)?;
        if wasm.len() > MAX_MODULE_BYTES {
            return Err(HostError::TooLarge(format!("module {} bytes", wasm.len())));
        }
        static_checks(wasm).map(|_| ())
    }

    /// Run all load-time checks, including the canonical engine's own
    /// validation, without executing.
    pub fn prepare(
        &self,
        id: &RecipeId,
        wasm: &[u8],
        profile: &RuntimeProfile,
    ) -> Result<(), HostError> {
        self.checks(id, wasm, profile)?;
        canonical_accepts(wasm)
    }

    /// Validate and run `wasm` on `input` (Borsh of `RecipeInput`). The verdict.
    ///
    /// `Err(HostError::Abstain)` means this machine could not complete the run
    /// (out of system memory, engine panic). It is not a verdict; another
    /// validator must decide.
    pub fn run(
        &self,
        id: &RecipeId,
        wasm: &[u8],
        input: &[u8],
        profile: &RuntimeProfile,
    ) -> Result<RunResult, HostError> {
        let key = (*id, profile.max_call_depth);
        let cached = self
            .cache
            .lock()
            .map_err(|_| HostError::Internal("cache poisoned".into()))?
            .get(&key);
        let (engine, module) = match cached {
            Some(em) => {
                if keccak256(wasm) != *id {
                    return Err(HostError::IdMismatch(*id));
                }
                check_profile(profile)?;
                em
            }
            None => {
                self.checks(id, wasm, profile)?;
                let engine = wasmi_engine(profile.max_call_depth);
                let module = translate(&engine, wasm)?;
                self.cache
                    .lock()
                    .map_err(|_| HostError::Internal("cache poisoned".into()))?
                    .insert(key, (engine.clone(), module.clone()), wasm.len());
                (engine, module)
            }
        };
        // Initial memory above the pinned limit is a deterministic trap on both
        // engines, decided here rather than by engine-specific instantiation errors.
        if initial_memory_bytes(wasm) > profile.max_memory_bytes {
            return fail(Outcome::Trap, 0);
        }
        // An engine panic must never take the validator down or become a verdict (L8-PANIC-5).
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute_wasmi(&engine, &module, input, profile)
        }))
        .unwrap_or_else(|_| Err(HostError::Abstain("engine panicked".into())))
    }

    /// Number of translated modules currently cached.
    pub fn cached_modules(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
    }
}

/// Store limits plus a flag for allocation failures of the host machine.
/// A `memory.grow` that the limits allow but the machine cannot satisfy would
/// otherwise return -1 like a normal refusal, and a recipe could branch on it,
/// so results would depend on validator RAM (L8-OOM-1). Such runs abstain.
struct AbstainingLimits {
    limits: wasmi::StoreLimits,
    machine_oom: bool,
}

impl wasmi::ResourceLimiter for AbstainingLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, wasmi_core::LimiterError> {
        self.limits.memory_growing(current, desired, maximum)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, wasmi_core::LimiterError> {
        self.limits.table_growing(current, desired, maximum)
    }

    fn memory_grow_failed(
        &mut self,
        error: &wasmi::errors::MemoryError,
    ) -> Result<(), wasmi_core::LimiterError> {
        if matches!(error, wasmi::errors::MemoryError::OutOfSystemMemory) {
            self.machine_oom = true;
            return Err(wasmi_core::LimiterError::ResourceLimiterDeniedAllocation);
        }
        Ok(())
    }

    fn table_grow_failed(
        &mut self,
        error: &wasmi::errors::TableError,
    ) -> Result<(), wasmi_core::LimiterError> {
        if matches!(error, wasmi::errors::TableError::OutOfSystemMemory) {
            self.machine_oom = true;
            return Err(wasmi_core::LimiterError::ResourceLimiterDeniedAllocation);
        }
        Ok(())
    }

    fn instances(&self) -> usize {
        self.limits.instances()
    }

    fn tables(&self) -> usize {
        self.limits.tables()
    }

    fn memories(&self) -> usize {
        self.limits.memories()
    }
}

fn execute_wasmi(
    engine: &wasmi::Engine,
    module: &wasmi::Module,
    input: &[u8],
    profile: &RuntimeProfile,
) -> Result<RunResult, HostError> {
    let limits = wasmi::StoreLimitsBuilder::new()
        .memory_size(usize::try_from(profile.max_memory_bytes).unwrap_or(usize::MAX))
        .table_elements(MAX_TABLE_ELEMENTS as usize)
        .instances(1)
        .tables(1)
        .memories(1)
        .build();
    let mut store = wasmi::Store::new(
        engine,
        AbstainingLimits {
            limits,
            machine_oom: false,
        },
    );
    store.limiter(|l| l);
    store
        .set_fuel(profile.fuel_limit)
        .map_err(|e| HostError::Internal(e.to_string()))?;
    let used = |s: &wasmi::Store<AbstainingLimits>| profile.fuel_limit - s.get_fuel().unwrap_or(0);
    // A failure caused by this machine (not by the wasm) is never a verdict.
    let fail_or_abstain = |s: &wasmi::Store<AbstainingLimits>, e: &wasmi::Error| {
        let machine = s.data().machine_oom
            || matches!(e.as_trap_code(), Some(wasmi::TrapCode::OutOfSystemMemory))
            || format!("{e:?}").contains("OutOfSystemMemory");
        if machine {
            return Err(HostError::Abstain(format!("out of system memory: {e}")));
        }
        let outcome = match e.as_trap_code() {
            Some(wasmi::TrapCode::OutOfFuel) => Outcome::OutOfFuel,
            _ => Outcome::Trap,
        };
        fail(outcome, used(s))
    };

    let instance = match wasmi::Instance::new(&mut store, module, &[]) {
        Ok(i) => i,
        Err(e) => return fail_or_abstain(&store, &e),
    };
    let abi = instance
        .get_typed_func::<(), u32>(&store, EXPORT_ABI_VERSION)
        .map_err(|e| HostError::BadAbi(e.to_string()))?;
    let alloc = instance
        .get_typed_func::<u32, u32>(&store, EXPORT_ALLOC)
        .map_err(|e| HostError::BadAbi(e.to_string()))?;
    let run = instance
        .get_typed_func::<(u32, u32), u64>(&store, EXPORT_RUN)
        .map_err(|e| HostError::BadAbi(e.to_string()))?;
    let memory = instance
        .get_memory(&store, "memory")
        .ok_or_else(|| HostError::BadAbi("no memory export".into()))?;

    match abi.call(&mut store, ()) {
        Ok(v) if v == ABI_VERSION => {}
        Ok(v) => return Err(HostError::BadAbi(format!("abi version {v}"))),
        Err(e) => return fail_or_abstain(&store, &e),
    }
    let len =
        u32::try_from(input.len()).map_err(|_| HostError::Internal("input too big".into()))?;
    let ptr = match alloc.call(&mut store, len) {
        Ok(p) => p,
        Err(e) => return fail_or_abstain(&store, &e),
    };
    if memory.write(&mut store, ptr as usize, input).is_err() {
        return fail(Outcome::Trap, used(&store));
    }
    let packed = match run.call(&mut store, (ptr, len)) {
        Ok(v) => v,
        Err(e) => return fail_or_abstain(&store, &e),
    };
    // Belt and braces: a grow refused by the machine but not trapped still abstains.
    if store.data().machine_oom {
        return Err(HostError::Abstain("out of system memory during run".into()));
    }
    let fuel_used = used(&store);
    let (optr, olen) = unpack_ptr_len(packed);
    if olen > profile.max_output_bytes {
        return fail(Outcome::OutputTooLarge, fuel_used);
    }
    // Bounds-check before allocating: the length is recipe-chosen (L9-F5).
    if u64::from(optr) + u64::from(olen) > memory.data_size(&store) as u64 {
        return fail(Outcome::BadOutput, fuel_used);
    }
    let mut out = vec![0u8; olen as usize];
    if memory.read(&store, optr as usize, &mut out).is_err() {
        return fail(Outcome::BadOutput, fuel_used);
    }
    decode_output(&out, profile, fuel_used)
}
